//! Параметры битового распределения аудиоблока: `baie`, `snroffste`,
//! `cplleake`, `deltbaie` (A/52 §5.3.3, §5.4.3.30–5.4.3.57).

use super::BlockContext;
use super::state::{ChannelSnr, CouplingStrategy, DeltaAllocation, FrameBlockState, SnrOffsets};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::bit_allocation::{
    BitAllocationParameters, DeltaSegment, DeltaSegments, LeakInitialization,
};

/// Состояния `cpldeltbae`/`deltbae` (Таблица 5.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaAllocationCode {
    /// `'00'` — оставить прежнее состояние.
    Reuse,
    /// `'01'` — далее идут новые сегменты.
    NewInformation,
    /// `'10'` — поправок нет.
    NoDelta,
}

impl DeltaAllocationCode {
    pub(crate) fn from_code(code: u8, context: &BlockContext) -> Result<Self, Ac3DecodeError> {
        match code {
            0 => Ok(Self::Reuse),
            1 => Ok(Self::NewInformation),
            2 => Ok(Self::NoDelta),
            // '11' — «the decoder should not decode audio, and should mute» (§5.4.3.48).
            _ => Err(context.violation(BitstreamViolation::ReservedDeltaBitAllocationState)),
        }
    }
}

/// Читает все поля параметров распределения и обновляет состояние кадра.
pub(crate) fn read_allocation_parameters(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    read_parametric_information(reader, context, state)?;
    read_snr_offsets(reader, context, coupling, state)?;
    if coupling.is_some() {
        read_coupling_leak(reader, context, state)?;
    }
    read_delta_bit_allocation(reader, context, coupling, state)
}

/// `baie`: `sdcycod`, `fdcycod`, `sgaincod`, `dbpbcod`, `floorcod`.
fn read_parametric_information(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
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
        return Err(context.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::BitAllocationParameters,
        )));
    }
    Ok(())
}

/// `snroffste`: `csnroffst`, coupling/fbw/LFE `fsnroffst` + `fgaincod`.
fn read_snr_offsets(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    if reader.read_flag().map_err(&overrun)? {
        let coarse_offset = reader.read_u8(6).map_err(&overrun)?;
        if coupling.is_some() {
            state.coupling_snr = Some(read_channel_snr(reader, context)?);
        }
        let mut full_bandwidth = [ChannelSnr {
            fine_offset: 0,
            fast_gain_code: 0,
        }; MAX_FULL_BANDWIDTH_CHANNELS];
        for snr in full_bandwidth.iter_mut().take(context.channel_count) {
            *snr = read_channel_snr(reader, context)?;
        }
        let lfe = if context.lfe_on {
            Some(read_channel_snr(reader, context)?)
        } else {
            None
        };
        state.snr_offsets = Some(SnrOffsets {
            coarse_offset,
            full_bandwidth,
            lfe,
        });
        return Ok(());
    }
    // Reuse: нужны и общие offset-ы, и (если coupling включён) offset coupling
    // канала — он забыт при включении coupling (§5.5).
    let coupling_ready = coupling.is_none() || state.coupling_snr.is_some();
    if state.snr_offsets.is_none() || !coupling_ready {
        return Err(context.violation(BitstreamViolation::InvalidReuse(ReusedElement::SnrOffsets)));
    }
    Ok(())
}

fn read_channel_snr(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
) -> Result<ChannelSnr, Ac3DecodeError> {
    let overrun = context.overrun();
    let fine_offset = reader.read_u8(4).map_err(&overrun)?;
    let fast_gain_code = reader.read_u8(3).map_err(&overrun)?;
    Ok(ChannelSnr {
        fine_offset,
        fast_gain_code,
    })
}

/// `cplleake`: `cplfleak`, `cplsleak` (условие 14 §7.10.2).
fn read_coupling_leak(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    if reader.read_flag().map_err(&overrun)? {
        let fast = reader.read_u8(3).map_err(&overrun)?;
        let slow = reader.read_u8(3).map_err(&overrun)?;
        state.coupling_leak = Some(LeakInitialization::from_codes(fast, slow));
    } else if state.coupling_leak.is_none() {
        return Err(context.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::CouplingLeak,
        )));
    }
    Ok(())
}

/// `deltbaie`: коды состояний, затем сегменты для «new info follows».
///
/// `deltbaie = 0` оставляет прежнее состояние (в блоке 0 — «поправок нет»,
/// §5.4.3.47). Код reuse `'00'` при пустом прошлом состоянии читается как
/// «поправок нет»: начальное состояние кадра определено спецификацией
/// (§7.2.2.6), а при включении coupling состояние coupling канала сброшено
/// (§5.5), так что условия 26–28 §7.10.2 не могут привести к чтению чужих данных.
fn read_delta_bit_allocation(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    if !reader.read_flag().map_err(&overrun)? {
        return Ok(());
    }
    let coupling_code = if coupling.is_some() {
        Some(DeltaAllocationCode::from_code(
            reader.read_u8(2).map_err(&overrun)?,
            context,
        )?)
    } else {
        None
    };
    let mut channel_codes = [DeltaAllocationCode::Reuse; MAX_FULL_BANDWIDTH_CHANNELS];
    for code in channel_codes.iter_mut().take(context.channel_count) {
        *code = DeltaAllocationCode::from_code(reader.read_u8(2).map_err(&overrun)?, context)?;
    }
    if let Some(code) = coupling_code {
        apply_delta_code(reader, context, code, &mut state.coupling_delta)?;
    }
    for (code, delta) in channel_codes
        .iter()
        .zip(state.channel_deltas.iter_mut())
        .take(context.channel_count)
    {
        apply_delta_code(reader, context, *code, delta)?;
    }
    Ok(())
}

/// Применяет код состояния delta bit allocation (общий с E-AC-3, Annex E Таблица E1.4).
pub(crate) fn apply_delta_code(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    code: DeltaAllocationCode,
    delta: &mut DeltaAllocation,
) -> Result<(), Ac3DecodeError> {
    match code {
        DeltaAllocationCode::Reuse => {}
        DeltaAllocationCode::NoDelta => *delta = DeltaAllocation::Inactive,
        DeltaAllocationCode::NewInformation => {
            *delta = DeltaAllocation::Active(read_delta_segments(reader, context)?)
        }
    }
    Ok(())
}

/// `deltnseg` (+1 сегмент), затем `deltoffst`(5), `deltlen`(4), `deltba`(3) на сегмент.
fn read_delta_segments(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
) -> Result<DeltaSegments, Ac3DecodeError> {
    let overrun = context.overrun();
    let segment_count = usize::from(reader.read_u8(3).map_err(&overrun)?) + 1;
    let mut segments = DeltaSegments::default();
    for _ in 0..segment_count {
        let offset = reader.read_u8(5).map_err(&overrun)?;
        let length = reader.read_u8(4).map_err(&overrun)?;
        let adjustment_code = reader.read_u8(3).map_err(&overrun)?;
        segments.push(DeltaSegment {
            offset,
            length,
            adjustment_code,
        });
    }
    Ok(segments)
}
