//! Параметры битового распределения аудиоблока E-AC-3: `baie` (или значения
//! по умолчанию), SNR offset-ы по стратегии кадра, fast gain, `convsnroffst`,
//! `cplleak`, delta bit allocation и `skipfld` (A/52 Annex E Таблица E1.4,
//! §2.3.2.3, §2.3.3.27–2.3.3.30).

use super::EnhancedBlockContext;
use super::state::{EnhancedBlockState, FastGainCodes};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::bit_allocation::{BitAllocationParameters, LeakInitialization};
use crate::syntax::audblk::allocation_fields::{DeltaAllocationCode, apply_delta_code};
use crate::syntax::audblk::skip_dummy_data;
use crate::syntax::eac3::audfrm::SnrOffsetStrategy;

/// Коды параметрического распределения при `bamode = 0` (Таблица E1.4):
/// `sdcycod = 2`, `fdcycod = 1`, `sgaincod = 1`, `dbpbcod = 2`, `floorcod = 7`.
const DEFAULT_ALLOCATION_CODES: [u8; 5] = [2, 1, 1, 2, 7];
/// Ширина `convsnroffst` (§2.3.3.30).
const CONVERTER_SNR_OFFSET_BITS: usize = 10;

/// Читает все поля распределения от `baie` до `skipfld` включительно.
pub(super) fn read_allocation_parameters(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling_in_use: bool,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    read_parametric_information(reader, context, state)?;
    read_snr_offsets(reader, context, coupling_in_use, state)?;
    read_fast_gains(reader, context, coupling_in_use, state)?;
    if context.independent {
        let overrun = context.base.overrun();
        if reader.read_flag().map_err(&overrun)? {
            reader
                .skip_bits(CONVERTER_SNR_OFFSET_BITS)
                .map_err(&overrun)?;
        }
    }
    if coupling_in_use {
        read_coupling_leak(reader, context, state)?;
    }
    if context.frame.syntax.delta_bit_allocation {
        read_delta_bit_allocation(reader, context, coupling_in_use, state)?;
    }
    if context.frame.syntax.skip_field {
        skip_dummy_data(reader, &context.base)?;
    }
    Ok(())
}

/// `baie` при `bamode = 1`, иначе коды по умолчанию в каждом блоке.
fn read_parametric_information(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    if !context.frame.syntax.bit_allocation {
        let [slow_decay, fast_decay, slow_gain, db_per_bit, floor] = DEFAULT_ALLOCATION_CODES;
        state.bit_allocation = Some(BitAllocationParameters::from_codes(
            slow_decay, fast_decay, slow_gain, db_per_bit, floor,
        ));
        return Ok(());
    }
    let overrun = context.base.overrun();
    if reader.read_flag().map_err(&overrun)? {
        let slow_decay = reader.read_u8(2).map_err(&overrun)?;
        let fast_decay = reader.read_u8(2).map_err(&overrun)?;
        let slow_gain = reader.read_u8(2).map_err(&overrun)?;
        let db_per_bit = reader.read_u8(2).map_err(&overrun)?;
        let floor = reader.read_u8(3).map_err(&overrun)?;
        state.bit_allocation = Some(BitAllocationParameters::from_codes(
            slow_decay, fast_decay, slow_gain, db_per_bit, floor,
        ));
    } else if state.bit_allocation.is_none() {
        // Кадр самодостаточен: в блоке 0 переиспользовать нечего.
        return Err(context.base.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::BitAllocationParameters,
        )));
    }
    Ok(())
}

/// SNR offset-ы по стратегии кадра (Таблица E2.9).
fn read_snr_offsets(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling_in_use: bool,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let channels = context.base.channel_count;
    let snr = &mut state.snr;
    match context.frame.snr_strategy {
        SnrOffsetStrategy::FrameWide { coarse, fine } => {
            snr.coarse = Some(coarse);
            snr.coupling = Some(fine);
            snr.channels
                .iter_mut()
                .take(channels)
                .for_each(|slot| *slot = Some(fine));
            snr.lfe = Some(fine);
        }
        strategy => {
            let overrun = context.base.overrun();
            let present = context.base.block.get() == 0 || reader.read_flag().map_err(&overrun)?;
            if present {
                snr.coarse = Some(reader.read_u8(6).map_err(&overrun)?);
                if strategy == SnrOffsetStrategy::SharedPerBlock {
                    let fine = reader.read_u8(4).map_err(&overrun)?;
                    snr.coupling = Some(fine);
                    snr.channels
                        .iter_mut()
                        .take(channels)
                        .for_each(|slot| *slot = Some(fine));
                    snr.lfe = Some(fine);
                } else {
                    if coupling_in_use {
                        snr.coupling = Some(reader.read_u8(4).map_err(&overrun)?);
                    }
                    for slot in snr.channels.iter_mut().take(channels) {
                        *slot = Some(reader.read_u8(4).map_err(&overrun)?);
                    }
                    if context.base.lfe_on {
                        snr.lfe = Some(reader.read_u8(4).map_err(&overrun)?);
                    }
                }
            }
        }
    }
    // Всё, что понадобится распределению, должно быть известно (стратегия 3:
    // coupling мог включиться в блоке без snroffste после блоков без coupling).
    let complete = snr.coarse.is_some()
        && snr.channels.iter().take(channels).all(Option::is_some)
        && (!coupling_in_use || snr.coupling.is_some())
        && (!context.base.lfe_on || snr.lfe.is_some());
    if !complete {
        return Err(context
            .base
            .violation(BitstreamViolation::InvalidReuse(ReusedElement::SnrOffsets)));
    }
    Ok(())
}

/// `fgaincode` + `cplfgaincod`/`fgaincod[ch]`/`lfefgaincod`, иначе код 4 (§2.3.3.28).
fn read_fast_gains(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling_in_use: bool,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    let present = context.frame.syntax.fast_gain && reader.read_flag().map_err(&overrun)?;
    let mut codes = FastGainCodes::defaults();
    if present {
        if coupling_in_use {
            codes.coupling = reader.read_u8(3).map_err(&overrun)?;
        }
        for code in codes.channels.iter_mut().take(context.base.channel_count) {
            *code = reader.read_u8(3).map_err(&overrun)?;
        }
        if context.base.lfe_on {
            codes.lfe = reader.read_u8(3).map_err(&overrun)?;
        }
    }
    state.fast_gain = codes;
    Ok(())
}

/// `cplleake` (неявно 1 при `firstcplleak`), `cplfleak`, `cplsleak`.
fn read_coupling_leak(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    let present = if state.first_coupling_leak {
        state.first_coupling_leak = false;
        true
    } else {
        reader.read_flag().map_err(&overrun)?
    };
    if present {
        let fast = reader.read_u8(3).map_err(&overrun)?;
        let slow = reader.read_u8(3).map_err(&overrun)?;
        state.coupling_leak = Some(LeakInitialization::from_codes(fast, slow));
    } else if state.coupling_leak.is_none() {
        return Err(context.base.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::CouplingLeak,
        )));
    }
    Ok(())
}

/// `deltbaie`, коды, затем сегменты — общие шаги AC-3 (`syntax::audblk::
/// allocation_fields`; «reuse» при пустом состоянии — «поправок нет», решение D1).
fn read_delta_bit_allocation(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling_in_use: bool,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    if !reader.read_flag().map_err(&overrun)? {
        return Ok(());
    }
    let base = &context.base;
    let coupling_code = if coupling_in_use {
        Some(DeltaAllocationCode::from_code(
            reader.read_u8(2).map_err(&overrun)?,
            base,
        )?)
    } else {
        None
    };
    let mut channel_codes = [DeltaAllocationCode::Reuse; MAX_FULL_BANDWIDTH_CHANNELS];
    for code in channel_codes.iter_mut().take(base.channel_count) {
        *code = DeltaAllocationCode::from_code(reader.read_u8(2).map_err(&overrun)?, base)?;
    }
    if let Some(code) = coupling_code {
        apply_delta_code(reader, base, code, &mut state.coupling_delta)?;
    }
    for (code, delta) in channel_codes
        .iter()
        .zip(state.channel_deltas.iter_mut())
        .take(base.channel_count)
    {
        apply_delta_code(reader, base, *code, delta)?;
    }
    Ok(())
}
