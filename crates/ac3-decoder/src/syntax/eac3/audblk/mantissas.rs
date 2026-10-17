//! Мантиссы аудиоблока E-AC-3: обычные (как в AC-3) и AHT
//! (A/52 Annex E Таблица E1.4 «quantized mantissa values», §3.4).
//!
//! Порядок — как в синтаксисе: fbw каналы по очереди; сразу после первого
//! канала в coupling — coupling канал; в конце — LFE. Канал с AHT читает
//! данные всех шести блоков в блоке 0 (`chahtinu = −1` после чтения), а в
//! блоках 1–5 его мантиссы берутся из [`AhtFrameStore`]. Группы bap 1/2/4
//! (общий `MantissaUnpacker` блока) продолжаются через каналы с AHT.
//!
//! Dither бинов AHT с hebap 0 (неоднозначность Annex E: §7.3.4 говорит о
//! «мантиссах», а у AHT мантиссы — коэффициенты DCT до §3.4.5): шум
//! подставляется в шесть коэффициентов DCT по `dithflag` блока 0 и проходит
//! через обратное DCT. Сверка средних спектров с ffmpeg и Nero дала
//! эквивалентную амплитуду эталона ≈ 0,49 в области DCT (допустимые §7.3.4
//! 0,5–0,75), тогда как dither в области MDCT потребовал бы ≈ 1,2 — вне
//! диапазона спецификации. Coupling канал с AHT, как в D1, получает dither
//! в каналах-получателях при восстановлении.

use super::EnhancedBlockContext;
use super::bit_allocation::{
    AllocationPointers, ExponentSetId, all_snr_offsets_are_zero, allocate_set, exponent_set,
};
use super::state::{EnhancedBlockState, EnhancedCouplingConfig};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation};
use crate::spectral::aht::{AhtChannelLayout, AhtMantissas, AhtReadError, read_aht_channel};
use crate::spectral::bit_allocation::AllocationPointerTable;
use crate::spectral::dither::{ChannelDither, DitherGenerator, DitherMode};
use crate::spectral::mantissa::{Mantissa, MantissaReadError, MantissaUnpacker};
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS, exponent_scale};
use crate::syntax::audblk::output::{ChannelSpectrum, DecodedAudioBlock, TransformLength};
use crate::syntax::audblk::state::ExponentSet;

/// Флаги блока, нужные при чтении мантисс.
#[derive(Debug, Clone, Copy)]
pub(super) struct ChannelFlags {
    /// `blksw[ch]`.
    pub(super) transform: [TransformLength; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `dithflag[ch]`.
    pub(super) dither: [ChannelDither; MAX_FULL_BANDWIDTH_CHANNELS],
}

/// Набор экспонент с AHT: указатели hebap блока 0 и мантиссы шести блоков.
#[derive(Debug, Clone)]
pub(crate) struct AhtSet {
    hebap: AllocationPointers,
    mantissas: AhtMantissas,
}

/// Данные AHT кадра (читаются в блоке 0, используются блоками 0–5).
///
/// Живёт в декодере и переиспользуется между кадрами: это ~43 КБ, которые
/// не нужно выделять на каждый кадр. Содержимое действительно только для
/// наборов с AHT текущего кадра (они перезаписываются в его блоке 0).
#[derive(Debug, Clone)]
pub(crate) struct AhtFrameStore {
    channels: [AhtSet; MAX_FULL_BANDWIDTH_CHANNELS],
    coupling: AhtSet,
    lfe: AhtSet,
}

impl AhtFrameStore {
    pub(crate) fn new() -> Box<Self> {
        let empty = || AhtSet {
            hebap: [0; COEFFICIENTS_PER_BLOCK],
            mantissas: AhtMantissas::new(),
        };
        Box::new(Self {
            channels: std::array::from_fn(|_| empty()),
            coupling: empty(),
            lfe: empty(),
        })
    }

    fn set_mut(&mut self, set: ExponentSetId) -> &mut AhtSet {
        match set {
            ExponentSetId::Channel(channel) => {
                &mut self.channels[channel.min(MAX_FULL_BANDWIDTH_CHANNELS - 1)]
            }
            ExponentSetId::Coupling => &mut self.coupling,
            ExponentSetId::Lfe => &mut self.lfe,
        }
    }
}

/// Мантиссы coupling канала блока: без экспоненты, с указателями (bap/hebap)
/// — нулевой указатель значит «dither канала-получателя».
pub(super) struct CouplingChannelMantissas {
    pub(super) mantissas: [f32; COEFFICIENTS_PER_BLOCK],
    pub(super) pointers: AllocationPointers,
}

/// Указатели всех обычных (не AHT) наборов блока.
struct BlockPointers {
    channels: [AllocationPointers; MAX_FULL_BANDWIDTH_CHANNELS],
    coupling: AllocationPointers,
    lfe: AllocationPointers,
}

/// Всё, что меняется при чтении мантисс блока.
pub(super) struct MantissaTargets<'a> {
    pub(super) state: &'a EnhancedBlockState,
    pub(super) aht: &'a mut AhtFrameStore,
    pub(super) dither: &'a mut DitherGenerator,
    pub(super) output: &'a mut DecodedAudioBlock,
    pub(super) coupling_channel: &'a mut CouplingChannelMantissas,
}

/// Считает указатели и читает все мантиссы блока: fbw и LFE — сразу в
/// коэффициенты `output` (×2^−exp), coupling канал — в `coupling_channel`.
pub(super) fn read_block_mantissas(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    flags: &ChannelFlags,
    targets: MantissaTargets<'_>,
) -> Result<(), Ac3DecodeError> {
    let MantissaTargets {
        state,
        aht,
        dither,
        output,
        coupling_channel,
    } = targets;
    let pointers = compute_pointers(context, coupling, state, aht)?;
    let mut unpacker = MantissaUnpacker::default();
    let mut coupling_read = false;
    for channel in 0..context.base.channel_count {
        let set = ExponentSetId::Channel(channel);
        let exponents = exponent_set(context, state, set)?;
        let spectrum = &mut output.full_bandwidth[channel];
        *spectrum = ChannelSpectrum::silent();
        spectrum.transform = flags.transform[channel];
        spectrum.end_bin = exponents.end;
        let channel_dither = flags.dither[channel];
        if context.frame.aht.channels[channel] {
            let zero_bits = ZeroBitSource {
                dither,
                channel: channel_dither,
            };
            let block_values =
                read_or_recall_aht(reader, context, exponents, zero_bits, aht.set_mut(set))?;
            apply_aht_block(block_values, exponents, &mut spectrum.coefficients);
        } else {
            let coefficients = &mut spectrum.coefficients;
            read_set_mantissas(
                reader,
                context,
                &mut unpacker,
                exponents,
                &pointers.channels[channel],
                |bin, mantissa| {
                    let value = match mantissa {
                        Mantissa::ZeroBits => dither.zero_bit_mantissa(channel_dither),
                        Mantissa::Value(value) => value,
                    };
                    coefficients[bin] = value * exponent_scale(exponents.exponents[bin]);
                },
            )?;
        }
        let coupled = coupling.is_some_and(|config| config.in_coupling[channel]);
        if coupled && !coupling_read {
            coupling_read = true;
            read_coupling_channel(
                reader,
                context,
                &mut unpacker,
                state,
                aht,
                &pointers,
                coupling_channel,
            )?;
        }
    }
    if context.base.lfe_on {
        let exponents = exponent_set(context, state, ExponentSetId::Lfe)?;
        output.lfe = ChannelSpectrum::silent();
        output.lfe.end_bin = exponents.end;
        // У LFE нет dithflag: нули — настоящие нули (как в D1).
        if context.frame.aht.lfe {
            let zero_bits = ZeroBitSource {
                dither,
                channel: ChannelDither::Off,
            };
            let block_values = read_or_recall_aht(
                reader,
                context,
                exponents,
                zero_bits,
                aht.set_mut(ExponentSetId::Lfe),
            )?;
            apply_aht_block(block_values, exponents, &mut output.lfe.coefficients);
        } else {
            let lfe = &mut output.lfe.coefficients;
            read_set_mantissas(
                reader,
                context,
                &mut unpacker,
                exponents,
                &pointers.lfe,
                |bin, mantissa| {
                    if let Mantissa::Value(value) = mantissa {
                        lfe[bin] = value * exponent_scale(exponents.exponents[bin]);
                    }
                },
            )?;
        }
    } else {
        output.lfe = ChannelSpectrum::silent();
    }
    Ok(())
}

/// Указатели bap обычных наборов; в блоке 0 — ещё и hebap наборов с AHT.
fn compute_pointers(
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    state: &EnhancedBlockState,
    aht: &mut AhtFrameStore,
) -> Result<BlockPointers, Ac3DecodeError> {
    let mut pointers = BlockPointers {
        channels: [[0; COEFFICIENTS_PER_BLOCK]; MAX_FULL_BANDWIDTH_CHANNELS],
        coupling: [0; COEFFICIENTS_PER_BLOCK],
        lfe: [0; COEFFICIENTS_PER_BLOCK],
    };
    let all_zero = all_snr_offsets_are_zero(context, coupling, state);
    let first_block = context.base.block.get() == 0;
    let aht_flags = context.frame.aht;
    let mut sets = Vec::with_capacity(MAX_FULL_BANDWIDTH_CHANNELS + 2);
    sets.extend(
        (0..context.base.channel_count)
            .map(|channel| (ExponentSetId::Channel(channel), aht_flags.channels[channel])),
    );
    if coupling.is_some() {
        sets.push((ExponentSetId::Coupling, aht_flags.coupling));
    }
    if context.base.lfe_on {
        sets.push((ExponentSetId::Lfe, aht_flags.lfe));
    }
    for (set, uses_aht) in sets {
        let (table, target) = if uses_aht {
            if !first_block {
                continue;
            }
            (
                AllocationPointerTable::HighEfficiency,
                &mut aht.set_mut(set).hebap,
            )
        } else {
            let target = match set {
                ExponentSetId::Channel(channel) => &mut pointers.channels[channel],
                ExponentSetId::Coupling => &mut pointers.coupling,
                ExponentSetId::Lfe => &mut pointers.lfe,
            };
            (AllocationPointerTable::Conventional, target)
        };
        if all_zero {
            // §7.2.2.1.1: все указатели нулевые, распределение не считается.
            *target = [0; COEFFICIENTS_PER_BLOCK];
        } else {
            allocate_set(context, state, set, table, target)?;
        }
    }
    Ok(pointers)
}

/// Мантиссы coupling канала блока (обычные или из AHT).
fn read_coupling_channel(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    unpacker: &mut MantissaUnpacker,
    state: &EnhancedBlockState,
    aht: &mut AhtFrameStore,
    pointers: &BlockPointers,
    coupling_channel: &mut CouplingChannelMantissas,
) -> Result<(), Ac3DecodeError> {
    let exponents = exponent_set(context, state, ExponentSetId::Coupling)?;
    coupling_channel.mantissas = [0.0; COEFFICIENTS_PER_BLOCK];
    if context.frame.aht.coupling {
        let set = aht.set_mut(ExponentSetId::Coupling);
        // Нулевые мантиссы coupling канала заполняются dither-ом каналов-получателей
        // при восстановлении (как в D1, §7.3.4): здесь они остаются нулями.
        let mut silent = DitherGenerator::new(DitherMode::Disabled);
        let zero_bits = ZeroBitSource {
            dither: &mut silent,
            channel: ChannelDither::Off,
        };
        let block_values = read_or_recall_aht(reader, context, exponents, zero_bits, set)?;
        let range = exponents.start..exponents.end.min(COEFFICIENTS_PER_BLOCK);
        coupling_channel.mantissas[range.clone()].copy_from_slice(&block_values.mantissas[range]);
        coupling_channel.pointers = set.hebap;
        return Ok(());
    }
    coupling_channel.pointers = pointers.coupling;
    let mantissas = &mut coupling_channel.mantissas;
    read_set_mantissas(
        reader,
        context,
        unpacker,
        exponents,
        &pointers.coupling,
        |bin, mantissa| {
            // Dither нулевых мантисс coupling канала — при восстановлении каналов (§7.3.4).
            if let Mantissa::Value(value) = mantissa {
                mantissas[bin] = value;
            }
        },
    )
}

/// Чем заполнять «предмантиссы» AHT без бит: генератор dither-а и
/// `dithflag` блока 0, в котором читаются данные AHT всего кадра.
struct ZeroBitSource<'a> {
    dither: &'a mut DitherGenerator,
    channel: ChannelDither,
}

/// Мантиссы MDCT этого блока для набора с AHT.
struct AhtBlockValues<'a> {
    mantissas: &'a [f32; COEFFICIENTS_PER_BLOCK],
}

/// В блоке 0 читает данные AHT набора; в любом блоке отдаёт значения текущего блока.
fn read_or_recall_aht<'a>(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    exponents: &ExponentSet,
    zero_bits: ZeroBitSource<'_>,
    set: &'a mut AhtSet,
) -> Result<AhtBlockValues<'a>, Ac3DecodeError> {
    let block = usize::from(context.base.block.get());
    if block == 0 {
        let layout = AhtChannelLayout {
            hebap: &set.hebap,
            start: exponents.start,
            end: exponents.end,
        };
        let ZeroBitSource { dither, channel } = zero_bits;
        let mut zero_bit_mantissa = || dither.zero_bit_aht_coefficient(channel);
        read_aht_channel(reader, &layout, &mut zero_bit_mantissa, &mut set.mantissas).map_err(
            |error| match error {
                AhtReadError::Overrun => Ac3DecodeError::BitstreamOverrun {
                    section: context.base.section(),
                },
                AhtReadError::GainCodeOutOfRange { code } => context
                    .base
                    .violation(BitstreamViolation::GainAdaptiveCodeOutOfRange { code }),
                AhtReadError::MantissaCodeOutOfRange { hebap, code } => context
                    .base
                    .violation(BitstreamViolation::MantissaCodeOutOfRange { bap: hebap, code }),
            },
        )?;
    }
    let values = set
        .mantissas
        .per_block
        .get(block)
        .unwrap_or(&set.mantissas.per_block[0]);
    Ok(AhtBlockValues { mantissas: values })
}

/// Коэффициенты блока канала с AHT: мантисса MDCT × 2^−exp. Dither бинов с
/// hebap 0 уже внесён в области AHT до обратного DCT (см. `read_or_recall_aht`).
fn apply_aht_block(
    values: AhtBlockValues<'_>,
    exponents: &ExponentSet,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) {
    let end = exponents.end.min(COEFFICIENTS_PER_BLOCK);
    let bins = coefficients
        .iter_mut()
        .zip(values.mantissas.iter().zip(&exponents.exponents))
        .take(end)
        .skip(exponents.start);
    for (coefficient, (&mantissa, &exponent)) in bins {
        *coefficient = mantissa * exponent_scale(exponent);
    }
}

/// Читает мантиссы бинов `start..end` набора по указателям `bap` и отдаёт
/// каждую в `store(bin, mantissa)`.
fn read_set_mantissas(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    unpacker: &mut MantissaUnpacker,
    exponents: &ExponentSet,
    bap: &AllocationPointers,
    mut store: impl FnMut(usize, Mantissa),
) -> Result<(), Ac3DecodeError> {
    let end = exponents.end.min(COEFFICIENTS_PER_BLOCK);
    for bin in exponents.start..end {
        let mantissa = unpacker
            .read(reader, bap[bin])
            .map_err(|error| match error {
                MantissaReadError::Overrun => Ac3DecodeError::BitstreamOverrun {
                    section: context.base.section(),
                },
                MantissaReadError::CodeOutOfRange { bap, code } => context
                    .base
                    .violation(BitstreamViolation::MantissaCodeOutOfRange { bap, code }),
            })?;
        store(bin, mantissa);
    }
    Ok(())
}
