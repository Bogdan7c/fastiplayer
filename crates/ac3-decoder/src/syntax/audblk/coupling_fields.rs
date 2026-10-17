//! Поля coupling аудиоблока: стратегия, координаты, фазовые флаги
//! (A/52 §5.3.3, Таблица 5.3; семантика §5.4.3.7–5.4.3.18, §7.4).

use super::BlockContext;
use super::state::{
    CouplingLayout, CouplingStatus, CouplingStrategy, FrameBlockState, StoredCouplingCoordinates,
};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::coupling::{MAX_COUPLING_SUBBANDS, coupling_coordinate};
use crate::syntax::bsi_values::AudioCodingMode;

/// Наименьшее число каналов в coupling (§5.4.3.9, §7.10.2 условие 2).
const MIN_COUPLED_CHANNELS: u8 = 2;

/// Читает `cplstre … cplbndstrc` и обновляет состояние кадра.
///
/// Возвращает стратегию текущего блока (`None` — coupling выключен).
pub(crate) fn read_coupling_strategy(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    state: &mut FrameBlockState,
) -> Result<Option<CouplingStrategy>, Ac3DecodeError> {
    let overrun = context.overrun();
    let strategy_present = reader.read_flag().map_err(&overrun)?;
    let current = if strategy_present {
        if reader.read_flag().map_err(&overrun)? {
            Some(read_new_strategy(reader, context)?)
        } else {
            None
        }
    } else {
        // cplstre = 0: стратегия прошлого блока (§5.4.3.7); в блоке 0 запрещено.
        match state.coupling {
            CouplingStatus::NotSignalled => {
                return Err(context.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::CouplingStrategy,
                )));
            }
            CouplingStatus::Off => None,
            CouplingStatus::On(strategy) => Some(strategy),
        }
    };
    if current.is_some() && !state.coupling_was_in_use() {
        state.forget_coupling_channel_state();
    }
    forget_coordinates_of_uncoupled_channels(current.as_ref(), state);
    state.coupling = match current {
        Some(strategy) => CouplingStatus::On(strategy),
        None => CouplingStatus::Off,
    };
    Ok(current)
}

/// `cplinu = 1`: `chincpl`, `phsflginu`, `cplbegf`, `cplendf`, `cplbndstrc`.
fn read_new_strategy(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
) -> Result<CouplingStrategy, Ac3DecodeError> {
    let overrun = context.overrun();
    if matches!(
        context.mode,
        AudioCodingMode::DualMono | AudioCodingMode::Mono
    ) {
        // Условие 22: coupling в 1+1 и 1/0 невозможен (§5.5).
        return Err(context.violation(BitstreamViolation::CouplingInSingleChannelMode));
    }
    let mut in_coupling = [false; MAX_FULL_BANDWIDTH_CHANNELS];
    for flag in in_coupling.iter_mut().take(context.channel_count) {
        *flag = reader.read_flag().map_err(&overrun)?;
    }
    let coupled_channels = in_coupling.iter().filter(|&&flag| flag).count() as u8;
    let phase_flags_in_use = if context.mode == AudioCodingMode::Stereo {
        reader.read_flag().map_err(&overrun)?
    } else {
        false
    };
    let begin_subband = reader.read_u8(4).map_err(&overrun)?;
    let end_code = reader.read_u8(4).map_err(&overrun)?;
    // Последняя sub-band = cplendf + 2; ncplsubnd = 3 + cplendf − cplbegf (§5.4.3.12).
    let last_subband = end_code + 2;
    if begin_subband > last_subband {
        return Err(
            context.violation(BitstreamViolation::InvertedCouplingRange {
                begin_subband,
                last_subband,
            }),
        );
    }
    let begin_subband = usize::from(begin_subband);
    let end_subband = usize::from(last_subband) + 1;
    let mut merged_with_previous = [false; MAX_COUPLING_SUBBANDS];
    for slot in merged_with_previous
        .iter_mut()
        .take(end_subband)
        .skip(begin_subband + 1)
    {
        *slot = reader.read_flag().map_err(&overrun)?;
    }
    if coupled_channels < MIN_COUPLED_CHANNELS {
        return Err(
            context.violation(BitstreamViolation::TooFewCoupledChannels { coupled_channels })
        );
    }
    Ok(CouplingStrategy {
        in_coupling,
        phase_flags_in_use,
        layout: CouplingLayout {
            begin_subband,
            end_subband,
            merged_with_previous,
        },
    })
}

/// Канал, вышедший из coupling, обязан прислать координаты при возвращении
/// (§5.4.3.14) — поэтому его старые координаты забываются.
fn forget_coordinates_of_uncoupled_channels(
    current: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) {
    for (channel, stored) in state.coupling_coordinates.iter_mut().enumerate() {
        let still_coupled = current.is_some_and(|strategy| strategy.in_coupling[channel]);
        if !still_coupled {
            *stored = None;
        }
    }
    if current.is_none() {
        state.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
    }
}

/// Читает `cplcoe … phsflg` для включённого coupling.
pub(crate) fn read_coupling_coordinates(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    strategy: &CouplingStrategy,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    let layout = strategy.layout;
    let band_of_subband = layout.band_of_each_subband();
    let band_count = layout.band_count();
    let mut any_new_coordinates_in_first_pair = false;
    for channel in 0..context.channel_count {
        if !strategy.in_coupling[channel] {
            continue;
        }
        if reader.read_flag().map_err(&overrun)? {
            let master = reader.read_u8(2).map_err(&overrun)?;
            let mut per_band = [0.0f32; MAX_COUPLING_SUBBANDS];
            for band_value in per_band.iter_mut().take(band_count) {
                let exponent = reader.read_u8(4).map_err(&overrun)?;
                let mantissa = reader.read_u8(4).map_err(&overrun)?;
                *band_value = coupling_coordinate(exponent, mantissa, master);
            }
            let mut per_subband = [0.0f32; MAX_COUPLING_SUBBANDS];
            for subband in layout.begin_subband..layout.end_subband {
                per_subband[subband] = per_band[band_of_subband[subband]];
            }
            state.coupling_coordinates[channel] = Some(StoredCouplingCoordinates {
                layout,
                per_subband,
            });
            if channel < 2 {
                any_new_coordinates_in_first_pair = true;
            }
        } else {
            // Reuse допустим, только если координаты есть и переданы под ту же
            // разметку (условия 4, 23, 24 §7.10.2).
            let reusable =
                state.coupling_coordinates[channel].is_some_and(|stored| stored.layout == layout);
            if !reusable {
                return Err(context.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::CouplingCoordinates,
                )));
            }
        }
    }
    if context.mode == AudioCodingMode::Stereo {
        read_phase_flags(
            reader,
            context,
            strategy,
            any_new_coordinates_in_first_pair,
            state,
        )?;
    }
    Ok(())
}

/// `phsflg[bnd]` (только 2/0): передаются при `phsflginu` и новых координатах
/// хотя бы одного из двух каналов; иначе действуют прежние (§7.4.1).
///
/// Синтаксис индексирует флаги по coupling полосам (`bnd < ncplbnd`), хотя
/// §7.4.1 говорит о sub-band-ах; принимаем синтаксис (он нормативен и задаёт
/// число бит) и раскладываем флаг полосы на все её sub-band-ы.
fn read_phase_flags(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    strategy: &CouplingStrategy,
    new_coordinates: bool,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    if !strategy.phase_flags_in_use {
        state.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
        return Ok(());
    }
    if !new_coordinates {
        return Ok(());
    }
    let layout = strategy.layout;
    let band_of_subband = layout.band_of_each_subband();
    let mut per_band = [false; MAX_COUPLING_SUBBANDS];
    for flag in per_band.iter_mut().take(layout.band_count()) {
        *flag = reader.read_flag().map_err(context.overrun())?;
    }
    state.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
    for subband in layout.begin_subband..layout.end_subband {
        state.phase_inverted[subband] = per_band[band_of_subband[subband]];
    }
    Ok(())
}
