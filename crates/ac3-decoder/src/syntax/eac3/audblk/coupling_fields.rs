//! Поля coupling аудиоблока E-AC-3: стратегия стандартного и улучшенного
//! coupling, координаты, фазовые флаги, амплитуды ECPL
//! (A/52 Annex E Таблица E1.4, §2.3.3.14–2.3.3.26, §3.3.1; ETSI TS 102 366
//! v1.4.1 §E.1.2.4 — синтаксис ECPL по этой, более новой, редакции).

use super::EnhancedBlockContext;
use super::state::{
    CouplingMode, EnhancedBlockState, EnhancedCouplingConfig, EnhancedCouplingStrategy,
    SpxStrategy, StoredEcplAmplitudes,
};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::coupling::{MAX_COUPLING_SUBBANDS, coupling_coordinate};
use crate::spectral::ecpl::EcplLayout;
use crate::syntax::audblk::state::{CouplingLayout, StoredCouplingCoordinates};
use crate::syntax::bsi_values::AudioCodingMode;
use crate::tables::eac3::coupling::{ECPL_LAST_UNMERGEABLE_SUBBAND, ECPL_SUBBANDS};

/// Наименьшее число каналов в coupling (A/52 §5.4.3.9).
const MIN_COUPLED_CHANNELS: usize = 2;
/// `ecpl_end_subbnd = ecplendf + 7` без SPX (§2.3.3.17).
const ECPL_END_CODE_BIAS: u8 = 7;
/// Ширина зарезервированной записи ECPL на полосу (ETSI §E.1.3.3.22: 9 бит).
const ECPL_RESERVED_BITS_PER_BAND: usize = 9;

/// `ecpl_begin_subbnd` по `ecplbegf` (§2.3.3.16).
const fn ecpl_begin_subband(begin_code: u8) -> u8 {
    if begin_code < 3 {
        begin_code * 2
    } else if begin_code < 13 {
        begin_code + 2
    } else {
        begin_code * 2 - 10
    }
}

/// Читает стратегию coupling блока (при `cplstre[blk] = 1`) или берёт прошлую.
pub(super) fn read_coupling_strategy(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    spx: Option<&SpxStrategy>,
    state: &mut EnhancedBlockState,
) -> Result<Option<EnhancedCouplingConfig>, Ac3DecodeError> {
    let block = usize::from(context.base.block.get());
    if !context.frame.coupling_strategy_present[block] {
        // cplinu[blk] = cplinu[blk−1]: действует прошлая стратегия (или выключено).
        return Ok(state.coupling);
    }
    if !context.frame.coupling_in_use[block] {
        state.leave_coupling();
        return Ok(None);
    }
    let overrun = context.base.overrun();
    let enhanced = reader.read_flag().map_err(&overrun)?;
    let mut in_coupling = [false; MAX_FULL_BANDWIDTH_CHANNELS];
    if context.base.mode == AudioCodingMode::Stereo {
        in_coupling[0] = true;
        in_coupling[1] = true;
    } else {
        for flag in in_coupling.iter_mut().take(context.base.channel_count) {
            *flag = reader.read_flag().map_err(&overrun)?;
        }
    }
    let mode = if enhanced {
        CouplingMode::Enhanced(read_enhanced_strategy(reader, context, spx, state)?)
    } else {
        read_standard_strategy(reader, context, spx, state)?
    };
    let coupled_channels = in_coupling.iter().filter(|&&flag| flag).count();
    if coupled_channels < MIN_COUPLED_CHANNELS {
        return Err(context
            .base
            .violation(BitstreamViolation::TooFewCoupledChannels {
                coupled_channels: coupled_channels as u8,
            }));
    }
    let config = EnhancedCouplingConfig { in_coupling, mode };
    state.coupling = Some(config);
    Ok(Some(config))
}

/// `phsflginu`, `cplbegf`, `cplendf` (или вывод из `spxbegf`), `cplbndstrc`.
fn read_standard_strategy(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    spx: Option<&SpxStrategy>,
    state: &mut EnhancedBlockState,
) -> Result<CouplingMode, Ac3DecodeError> {
    let overrun = context.base.overrun();
    let phase_flags_in_use =
        context.base.mode == AudioCodingMode::Stereo && reader.read_flag().map_err(&overrun)?;
    let begin_subband = reader.read_u8(4).map_err(&overrun)?;
    // Sub-band за последним: cplendf + 3; с SPX coupling кончается там, где
    // начинается SPX (§3.3.1: cplendf = spxbegf − 2 или 2·spxbegf − 7).
    let end_subband = match spx {
        None => reader.read_u8(4).map_err(&overrun)? + 3,
        Some(strategy) => super::spx_fields::spx_begin_subband(strategy.begin_code) - 1,
    };
    if begin_subband + 1 > end_subband {
        return Err(context
            .base
            .violation(BitstreamViolation::InvertedCouplingRange {
                begin_subband,
                last_subband: end_subband.saturating_sub(1),
            }));
    }
    let begin = usize::from(begin_subband);
    let end = usize::from(end_subband).min(MAX_COUPLING_SUBBANDS);
    if reader.read_flag().map_err(&overrun)? {
        // cplbndstrc[bnd] для bnd = 1 … ncplsubnd − 1 — относительно cplbegf.
        for slot in state
            .coupling_band_structure
            .iter_mut()
            .take(end)
            .skip(begin + 1)
        {
            *slot = reader.read_flag().map_err(&overrun)?;
        }
    }
    let mut merged_with_previous = state.coupling_band_structure;
    merged_with_previous[begin.min(MAX_COUPLING_SUBBANDS - 1)] = false;
    Ok(CouplingMode::Standard {
        phase_flags_in_use,
        layout: CouplingLayout {
            begin_subband: begin,
            end_subband: end,
            merged_with_previous,
        },
    })
}

/// `ecplbegf`, `ecplendf` (или вывод из `spxbegf`), `ecplbndstrc`.
fn read_enhanced_strategy(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    spx: Option<&SpxStrategy>,
    state: &mut EnhancedBlockState,
) -> Result<EnhancedCouplingStrategy, Ac3DecodeError> {
    let overrun = context.base.overrun();
    let begin_code = reader.read_u8(4).map_err(&overrun)?;
    let begin_subband = ecpl_begin_subband(begin_code);
    let end_subband = match spx {
        None => reader.read_u8(4).map_err(&overrun)? + ECPL_END_CODE_BIAS,
        // Sub-band ECPL n ≥ 4 совпадает с sub-band-ом SPX n − 3 (Таблицы E3.7, E3.13).
        Some(strategy) => super::spx_fields::spx_begin_subband(strategy.begin_code) + 3,
    };
    if reader.read_flag().map_err(&overrun)? {
        let first = (ECPL_LAST_UNMERGEABLE_SUBBAND + 1).max(usize::from(begin_subband) + 1);
        for slot in state
            .ecpl_band_structure
            .iter_mut()
            .take(usize::from(end_subband).min(ECPL_SUBBANDS))
            .skip(first)
        {
            *slot = reader.read_flag().map_err(&overrun)?;
        }
    }
    if begin_subband >= end_subband || usize::from(end_subband) > ECPL_SUBBANDS {
        return Err(context
            .base
            .violation(BitstreamViolation::InvertedEnhancedCouplingRange {
                begin_subband,
                end_subband,
            }));
    }
    Ok(EnhancedCouplingStrategy {
        begin_code,
        layout: EcplLayout::new(
            usize::from(begin_subband),
            usize::from(end_subband),
            state.ecpl_band_structure,
        ),
    })
}

/// Координаты стандартного coupling или амплитуды ECPL (при `cplinu = 1`).
pub(super) fn read_coupling_parameters(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    config: &EnhancedCouplingConfig,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    match config.mode {
        CouplingMode::Standard {
            phase_flags_in_use,
            layout,
        } => read_standard_coordinates(reader, context, config, phase_flags_in_use, layout, state),
        CouplingMode::Enhanced(strategy) => {
            read_enhanced_amplitudes(reader, context, config, &strategy.layout, state)
        }
    }
}

/// `cplcoe` (неявно 1 при `firstcplcos`), `mstrcplco`, `cplcoexp/cplcomant`, `phsflg`.
fn read_standard_coordinates(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    config: &EnhancedCouplingConfig,
    phase_flags_in_use: bool,
    layout: CouplingLayout,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    let band_of_subband = layout.band_of_each_subband();
    let band_count = layout.band_count();
    let mut new_in_first_pair = false;
    for channel in 0..context.base.channel_count {
        if !config.in_coupling[channel] {
            state.first_coupling_coordinates[channel] = true;
            state.coupling_coordinates[channel] = None;
            continue;
        }
        let new_coordinates = take_first_or_read(
            reader,
            context,
            &mut state.first_coupling_coordinates[channel],
        )?;
        if !new_coordinates {
            let reusable =
                state.coupling_coordinates[channel].is_some_and(|stored| stored.layout == layout);
            if !reusable {
                return Err(context.base.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::CouplingCoordinates,
                )));
            }
            continue;
        }
        let master = reader.read_u8(2).map_err(&overrun)?;
        let mut per_band = [0.0f32; MAX_COUPLING_SUBBANDS];
        for value in per_band.iter_mut().take(band_count) {
            let exponent = reader.read_u8(4).map_err(&overrun)?;
            let mantissa = reader.read_u8(4).map_err(&overrun)?;
            *value = coupling_coordinate(exponent, mantissa, master);
        }
        let mut per_subband = [0.0f32; MAX_COUPLING_SUBBANDS];
        for subband in layout.begin_subband..layout.end_subband {
            per_subband[subband] = per_band[band_of_subband[subband]];
        }
        state.coupling_coordinates[channel] = Some(StoredCouplingCoordinates {
            layout,
            per_subband,
        });
        new_in_first_pair |= channel < 2;
    }
    if context.base.mode != AudioCodingMode::Stereo {
        return Ok(());
    }
    if !phase_flags_in_use {
        state.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
        return Ok(());
    }
    if new_in_first_pair {
        // phsflg[bnd] по coupling полосам раскладываются на sub-band-ы (как в D1).
        let mut per_band = [false; MAX_COUPLING_SUBBANDS];
        for flag in per_band.iter_mut().take(band_count) {
            *flag = reader.read_flag().map_err(&overrun)?;
        }
        state.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
        for subband in layout.begin_subband..layout.end_subband {
            state.phase_inverted[subband] = per_band[band_of_subband[subband]];
        }
    }
    Ok(())
}

/// ECPL по ETSI TS 102 366 v1.4.1 §E.1.2.4: 1 зарезервированный бит, затем
/// для каждого coupled канала `ecplparam1e` (неявно 1 при `firstcplcos`),
/// `rsvdfieldse` (у всех, кроме первого coupled), амплитуды, зарезервированные
/// `9·(necplbnd − 1)` бит и ещё 1 зарезервированный бит.
///
/// ATSC A/52:2012 на этих местах передаёт `ecplangleintrp`, углы, «хаос» и
/// `ecpltrans` (9 бит на полосу, `necplbnd` полос); длины расходятся на 9 бит.
/// Принята редакция ETSI как более новая (см. модуль `spectral::ecpl`).
fn read_enhanced_amplitudes(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    config: &EnhancedCouplingConfig,
    layout: &EcplLayout,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    let band_count = layout.band_count();
    reader.skip_bits(1).map_err(&overrun)?;
    let first_coupled = config.in_coupling.iter().position(|&flag| flag);
    for channel in 0..context.base.channel_count {
        if !config.in_coupling[channel] {
            state.first_coupling_coordinates[channel] = true;
            state.ecpl_amplitudes[channel] = None;
            continue;
        }
        let follows_first = first_coupled.is_some_and(|first| channel > first);
        let (new_amplitudes, reserved_present) = if state.first_coupling_coordinates[channel] {
            state.first_coupling_coordinates[channel] = false;
            (true, follows_first)
        } else {
            let amplitudes = reader.read_flag().map_err(&overrun)?;
            let reserved = follows_first && reader.read_flag().map_err(&overrun)?;
            (amplitudes, reserved)
        };
        if new_amplitudes {
            let mut per_band = [0.0f32; ECPL_SUBBANDS];
            for value in per_band.iter_mut().take(band_count) {
                *value = crate::tables::eac3::coupling::ecpl_amplitude(
                    reader.read_u8(5).map_err(&overrun)?,
                );
            }
            state.ecpl_amplitudes[channel] = Some(StoredEcplAmplitudes {
                layout: *layout,
                per_band,
            });
        } else if !state.ecpl_amplitudes[channel].is_some_and(|stored| stored.layout == *layout) {
            return Err(context.base.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::EnhancedCouplingAmplitudes,
            )));
        }
        if reserved_present {
            reader
                .skip_bits(ECPL_RESERVED_BITS_PER_BAND * band_count.saturating_sub(1))
                .map_err(&overrun)?;
        }
        if follows_first {
            reader.skip_bits(1).map_err(&overrun)?;
        }
    }
    Ok(())
}

/// `first*cos = 1` → «новые данные» без чтения бита (и сброс флага), иначе бит из потока.
fn take_first_or_read(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    first: &mut bool,
) -> Result<bool, Ac3DecodeError> {
    if *first {
        *first = false;
        return Ok(true);
    }
    reader.read_flag().map_err(context.base.overrun())
}

#[cfg(test)]
mod tests {
    use super::ecpl_begin_subband;

    /// Таблица E3.8: ecplbegf 0–15 → sub-band-ы 0, 2, 4, 5–14, 16, 18, 20.
    #[test]
    fn ecpl_begin_codes_follow_table_e3_8() {
        let begins: Vec<u8> = (0..16).map(ecpl_begin_subband).collect();
        assert_eq!(
            begins,
            [0, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 18, 20]
        );
    }
}
