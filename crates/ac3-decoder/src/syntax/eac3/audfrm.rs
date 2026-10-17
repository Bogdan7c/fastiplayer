//! `audfrm` — параметры всего кадра E-AC-3 (A/52 Annex E §2.2.3,
//! Таблица E1.3, семантика §2.3.2; ETSI TS 102 366 v1.4.1 §E.1.2.3, §E.1.3.2).
//!
//! Здесь E-AC-3 выносит на уровень кадра то, что AC-3 передаёт в каждом
//! блоке: флаги наличия синтаксиса (`blkswe`, `bamode`, …), включение
//! coupling по блокам, стратегии экспонент (по блокам или одним 5-битным
//! кодом кадра, Таблица E2.10), флаги AHT, SNR offset кадра, параметры
//! подавления пред-шума (TPNP) и ослабления SPX.

use crate::bitstream::{BitReader, ReadPastLimit};
use crate::error::{Ac3DecodeError, BitstreamViolation, FrameSection};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::exponents::ExponentStrategy;
use crate::syntax::syncframe::AUDIO_BLOCKS_PER_SYNCFRAME;
use crate::tables::eac3::exponent_strategy::frame_exponent_strategies;

/// Шаг `transprocloc` в сэмплах (§2.3.2.22: «multiplying this value by 4»).
const TRANSIENT_LOCATION_STEP_SAMPLES: u16 = 4;
/// Ширина 5-битных кодов стратегий кадра и конвертера (§2.3.2.12–2.3.2.15).
const FRAME_STRATEGY_CODE_BITS: u32 = 5;
/// Постоянная часть ширины записи `blkstrtinfo` на блок (§2.3.2.27).
const BLOCK_START_BITS_BASE: usize = 4;

/// Стратегия SNR offset (`snroffststr`, Таблица E2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnrOffsetStrategy {
    /// Стратегия 1: один `frmcsnroffst`/`frmfsnroffst` на весь кадр.
    FrameWide { coarse: u8, fine: u8 },
    /// Стратегия 2: `csnroffst` + общий `blkfsnroffst` в блоках с `snroffste`.
    SharedPerBlock,
    /// Стратегия 3: `csnroffst` + свой fine offset у каждого канала.
    ChannelPerBlock,
}

/// Параметры подавления пред-шума перед транзиентом одного канала
/// (`chintransproc = 1`, §2.3.2.21–2.3.2.23). Обработка во временной
/// области (§3.7) делается при синтезе PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransientPreNoise {
    /// Положение транзиента от начала кадра в сэмплах (`transprocloc × 4`).
    pub location_samples: u16,
    /// Длина обработки в сэмплах (`transproclen`).
    pub length_samples: u8,
}

/// Публичная сводка `audfrm`: инструменты кадра, о которых полезно знать снаружи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AudioFrameSummary {
    /// `ahte`: хотя бы один канал кодирован AHT.
    pub adaptive_hybrid_transform: bool,
    /// TPNP по fbw каналам (порядок Таблицы 5.8); `None` — без обработки.
    pub transient_pre_noise: [Option<TransientPreNoise>; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `spxattencod` по fbw каналам, если ослабление SPX включено.
    pub spectral_extension_attenuation: [Option<u8>; MAX_FULL_BANDWIDTH_CHANNELS],
}

/// Каналы с AHT в кадре (`cplahtinu`, `chahtinu[ch]`, `lfeahtinu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AhtChannels {
    pub(crate) coupling: bool,
    pub(crate) channels: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) lfe: bool,
}

/// Флаги синтаксиса блоков, общие для кадра (§2.3.2.5–2.3.2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockSyntaxFlags {
    /// `blkswe`.
    pub(crate) block_switch: bool,
    /// `dithflage`.
    pub(crate) dither_flags: bool,
    /// `bamode`.
    pub(crate) bit_allocation: bool,
    /// `frmfgaincode`.
    pub(crate) fast_gain: bool,
    /// `dbaflde`.
    pub(crate) delta_bit_allocation: bool,
    /// `skipflde`.
    pub(crate) skip_field: bool,
}

/// Разобранный `audfrm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioFrame {
    pub(crate) syntax: BlockSyntaxFlags,
    pub(crate) snr_strategy: SnrOffsetStrategy,
    /// `cplstre[blk]` (для блока 0 — всегда `true`).
    pub(crate) coupling_strategy_present: [bool; AUDIO_BLOCKS_PER_SYNCFRAME],
    /// `cplinu[blk]`.
    pub(crate) coupling_in_use: [bool; AUDIO_BLOCKS_PER_SYNCFRAME],
    /// `cplexpstr[blk]` (reuse там, где coupling выключен).
    pub(crate) coupling_exponent_strategy: [ExponentStrategy; AUDIO_BLOCKS_PER_SYNCFRAME],
    /// `chexpstr[blk][ch]`.
    pub(crate) channel_exponent_strategy:
        [[ExponentStrategy; MAX_FULL_BANDWIDTH_CHANNELS]; AUDIO_BLOCKS_PER_SYNCFRAME],
    /// `lfeexpstr[blk]`.
    pub(crate) lfe_exponent_strategy: [ExponentStrategy; AUDIO_BLOCKS_PER_SYNCFRAME],
    pub(crate) aht: AhtChannels,
    pub(crate) summary: AudioFrameSummary,
}

/// Параметры кадра, от которых зависит синтаксис `audfrm`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioFrameContext {
    pub(crate) audio_blocks: usize,
    pub(crate) channel_count: usize,
    /// `acmod > 1` — coupling возможен.
    pub(crate) coupling_possible: bool,
    pub(crate) lfe_on: bool,
    pub(crate) independent: bool,
    /// `frmsiz + 1`.
    pub(crate) frame_words: usize,
}

impl AudioFrameContext {
    const fn six_blocks(&self) -> bool {
        self.audio_blocks == AUDIO_BLOCKS_PER_SYNCFRAME
    }
}

fn overrun(_: ReadPastLimit) -> Ac3DecodeError {
    Ac3DecodeError::BitstreamOverrun {
        section: FrameSection::AudioFrame,
    }
}

/// Читает `audfrm` целиком.
pub(crate) fn read_audio_frame(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
) -> Result<AudioFrame, Ac3DecodeError> {
    let (exponent_strategy_syntax, aht_enabled) = if context.six_blocks() {
        (
            reader.read_flag().map_err(overrun)?,
            reader.read_flag().map_err(overrun)?,
        )
    } else {
        (true, false)
    };
    let snr_strategy_code = reader.read_u8(2).map_err(overrun)?;
    let transient_processing = reader.read_flag().map_err(overrun)?;
    let syntax_bits = read_flags::<6>(reader)?;
    let syntax = BlockSyntaxFlags {
        block_switch: syntax_bits[0],
        dither_flags: syntax_bits[1],
        bit_allocation: syntax_bits[2],
        fast_gain: syntax_bits[3],
        delta_bit_allocation: syntax_bits[4],
        skip_field: syntax_bits[5],
    };
    let spx_attenuation = reader.read_flag().map_err(overrun)?;
    let mut frame = AudioFrame {
        syntax,
        snr_strategy: SnrOffsetStrategy::SharedPerBlock,
        coupling_strategy_present: [false; AUDIO_BLOCKS_PER_SYNCFRAME],
        coupling_in_use: [false; AUDIO_BLOCKS_PER_SYNCFRAME],
        coupling_exponent_strategy: [ExponentStrategy::Reuse; AUDIO_BLOCKS_PER_SYNCFRAME],
        channel_exponent_strategy: [[ExponentStrategy::Reuse; MAX_FULL_BANDWIDTH_CHANNELS];
            AUDIO_BLOCKS_PER_SYNCFRAME],
        lfe_exponent_strategy: [ExponentStrategy::Reuse; AUDIO_BLOCKS_PER_SYNCFRAME],
        aht: AhtChannels::default(),
        summary: AudioFrameSummary {
            adaptive_hybrid_transform: aht_enabled,
            transient_pre_noise: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            spectral_extension_attenuation: [None; MAX_FULL_BANDWIDTH_CHANNELS],
        },
    };
    read_coupling_flags(reader, context, &mut frame)?;
    read_exponent_strategies(reader, context, exponent_strategy_syntax, &mut frame)?;
    skip_converter_strategies(reader, context)?;
    if aht_enabled {
        read_aht_flags(reader, context, &mut frame)?;
    }
    frame.snr_strategy = match snr_strategy_code {
        0 => SnrOffsetStrategy::FrameWide {
            coarse: reader.read_u8(6).map_err(overrun)?,
            fine: reader.read_u8(4).map_err(overrun)?,
        },
        1 => SnrOffsetStrategy::SharedPerBlock,
        2 => SnrOffsetStrategy::ChannelPerBlock,
        _ => {
            return Err(Ac3DecodeError::InvalidBitstream {
                section: FrameSection::AudioFrame,
                violation: BitstreamViolation::ReservedSnrOffsetStrategy,
            });
        }
    };
    if transient_processing {
        read_transient_pre_noise(reader, context, &mut frame.summary)?;
    }
    if spx_attenuation {
        read_spx_attenuation(reader, context, &mut frame.summary)?;
    }
    skip_block_start_information(reader, context)?;
    Ok(frame)
}

fn read_flags<const COUNT: usize>(
    reader: &mut BitReader<'_>,
) -> Result<[bool; COUNT], Ac3DecodeError> {
    let mut flags = [false; COUNT];
    for flag in &mut flags {
        *flag = reader.read_flag().map_err(overrun)?;
    }
    Ok(flags)
}

/// `cplinu[0]`, затем `cplstre[blk]`/`cplinu[blk]` (только при `acmod > 1`).
fn read_coupling_flags(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
    frame: &mut AudioFrame,
) -> Result<(), Ac3DecodeError> {
    // Блок 0 всегда «передаёт стратегию» (cplstre[0] = 1), даже при acmod ≤ 1.
    frame.coupling_strategy_present[0] = true;
    if !context.coupling_possible {
        return Ok(());
    }
    frame.coupling_in_use[0] = reader.read_flag().map_err(overrun)?;
    for block in 1..context.audio_blocks {
        let present = reader.read_flag().map_err(overrun)?;
        frame.coupling_strategy_present[block] = present;
        frame.coupling_in_use[block] = if present {
            reader.read_flag().map_err(overrun)?
        } else {
            frame.coupling_in_use[block - 1]
        };
    }
    Ok(())
}

/// Стратегии экспонент: по блокам (`expstre = 1`) или кодом кадра (Таблица E2.10).
fn read_exponent_strategies(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
    per_block_syntax: bool,
    frame: &mut AudioFrame,
) -> Result<(), Ac3DecodeError> {
    let blocks = context.audio_blocks;
    if per_block_syntax {
        for block in 0..blocks {
            if frame.coupling_in_use[block] {
                frame.coupling_exponent_strategy[block] =
                    ExponentStrategy::from_code(reader.read_u8(2).map_err(overrun)?);
            }
            for strategy in frame.channel_exponent_strategy[block]
                .iter_mut()
                .take(context.channel_count)
            {
                *strategy = ExponentStrategy::from_code(reader.read_u8(2).map_err(overrun)?);
            }
        }
    } else {
        // Только при шести блоках: таблица E2.10 задана для блоков 0–5.
        let coupled_blocks = frame
            .coupling_in_use
            .iter()
            .take(blocks)
            .filter(|&&on| on)
            .count();
        if context.coupling_possible && coupled_blocks > 0 {
            let strategies = frame_exponent_strategies(
                reader.read_u8(FRAME_STRATEGY_CODE_BITS).map_err(overrun)?,
            );
            let per_block = frame
                .coupling_exponent_strategy
                .iter_mut()
                .zip(frame.coupling_in_use)
                .zip(strategies)
                .take(blocks);
            for ((slot, in_use), strategy) in per_block {
                if in_use {
                    *slot = strategy;
                }
            }
        }
        for channel in 0..context.channel_count {
            let strategies = frame_exponent_strategies(
                reader.read_u8(FRAME_STRATEGY_CODE_BITS).map_err(overrun)?,
            );
            let per_block = frame
                .channel_exponent_strategy
                .iter_mut()
                .zip(strategies)
                .take(blocks);
            for (block_strategies, strategy) in per_block {
                block_strategies[channel] = strategy;
            }
        }
    }
    if context.lfe_on {
        for strategy in frame.lfe_exponent_strategy.iter_mut().take(blocks) {
            *strategy = ExponentStrategy::from_lfe_flag(reader.read_flag().map_err(overrun)?);
        }
    }
    Ok(())
}

/// `convexpstre`/`convexpstr[ch]` — нужны только конвертеру в AC-3 (§2.3.2.14).
fn skip_converter_strategies(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
) -> Result<(), Ac3DecodeError> {
    if !context.independent {
        return Ok(());
    }
    let present = context.six_blocks() || reader.read_flag().map_err(overrun)?;
    if present {
        reader
            .skip_bits(context.channel_count * FRAME_STRATEGY_CODE_BITS as usize)
            .map_err(overrun)?;
    }
    Ok(())
}

/// `cplahtinu`, `chahtinu[ch]`, `lfeahtinu` с условиями `ncplregs`/`nchregs`/
/// `nlferegs == 1` (§3.4.2: «число раз, когда экспоненты передаются»).
fn read_aht_flags(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
    frame: &mut AudioFrame,
) -> Result<(), Ac3DecodeError> {
    let blocks = context.audio_blocks;
    let coupled_blocks = frame
        .coupling_in_use
        .iter()
        .take(blocks)
        .filter(|&&on| on)
        .count();
    let coupling_regions = (0..blocks)
        .filter(|&block| {
            frame.coupling_strategy_present[block]
                || frame.coupling_exponent_strategy[block] != ExponentStrategy::Reuse
        })
        .count();
    if coupled_blocks == AUDIO_BLOCKS_PER_SYNCFRAME && coupling_regions == 1 {
        frame.aht.coupling = reader.read_flag().map_err(overrun)?;
    }
    for channel in 0..context.channel_count {
        let regions = (0..blocks)
            .filter(|&block| {
                frame.channel_exponent_strategy[block][channel] != ExponentStrategy::Reuse
            })
            .count();
        if regions == 1 {
            frame.aht.channels[channel] = reader.read_flag().map_err(overrun)?;
        }
    }
    if context.lfe_on {
        let regions = (0..blocks)
            .filter(|&block| frame.lfe_exponent_strategy[block] != ExponentStrategy::Reuse)
            .count();
        if regions == 1 {
            frame.aht.lfe = reader.read_flag().map_err(overrun)?;
        }
    }
    Ok(())
}

/// `chintransproc[ch]` + `transprocloc` (10) + `transproclen` (8).
fn read_transient_pre_noise(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
    summary: &mut AudioFrameSummary,
) -> Result<(), Ac3DecodeError> {
    for slot in summary
        .transient_pre_noise
        .iter_mut()
        .take(context.channel_count)
    {
        if reader.read_flag().map_err(overrun)? {
            let location = reader.read_bits(10).map_err(overrun)?;
            let length = reader.read_u8(8).map_err(overrun)?;
            *slot = Some(TransientPreNoise {
                location_samples: location * TRANSIENT_LOCATION_STEP_SAMPLES,
                length_samples: length,
            });
        }
    }
    Ok(())
}

/// `chinspxatten[ch]` + `spxattencod[ch]` (5).
fn read_spx_attenuation(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
    summary: &mut AudioFrameSummary,
) -> Result<(), Ac3DecodeError> {
    for slot in summary
        .spectral_extension_attenuation
        .iter_mut()
        .take(context.channel_count)
    {
        if reader.read_flag().map_err(overrun)? {
            *slot = Some(reader.read_u8(5).map_err(overrun)?);
        }
    }
    Ok(())
}

/// `blkstrtinfoe` + `blkstrtinfo` длиной `(numblks − 1)·(4 + ⌈log2(frmsiz + 1)⌉)`
/// (§2.3.2.27). Позиции блоков декодеру не нужны: блоки читаются подряд.
fn skip_block_start_information(
    reader: &mut BitReader<'_>,
    context: &AudioFrameContext,
) -> Result<(), Ac3DecodeError> {
    if context.audio_blocks == 1 || !reader.read_flag().map_err(overrun)? {
        return Ok(());
    }
    let bits_per_block = BLOCK_START_BITS_BASE + ceil_log2(context.frame_words);
    reader
        .skip_bits((context.audio_blocks - 1) * bits_per_block)
        .map_err(overrun)
}

/// `⌈log2(n)⌉` для n ≥ 1 (для n = 1 — 0).
fn ceil_log2(value: usize) -> usize {
    let mut bits = 0usize;
    while (1usize << bits) < value {
        bits += 1;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::ceil_log2;

    #[test]
    fn ceil_log2_matches_spec_example() {
        // §2.3.2.27: ceiling(2.1) = 3; степени двойки не округляются вверх.
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(1024), 10);
        assert_eq!(ceil_log2(1025), 11);
    }
}
