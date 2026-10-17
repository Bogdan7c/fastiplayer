//! Поля спектрального расширения аудиоблока E-AC-3: стратегия и координаты
//! (A/52 Annex E Таблица E1.4, семантика §2.3.3.1–2.3.3.13, формат §3.6.3).

use super::EnhancedBlockContext;
use super::state::{EnhancedBlockState, SpxStrategy, StoredSpxBands};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::spx::{SpxBandLayout, SpxChannelBands, blending_factors, spx_coordinate};
use crate::syntax::bsi_values::AudioCodingMode;
use crate::tables::eac3::spx::SPX_SUBBANDS;

/// `spxbegf`, начиная с которого шаг sub-band-ов удваивается (§2.3.3.5).
const SPX_BEGIN_LINEAR_CODES: u8 = 6;
/// `spxendf`, начиная с которого шаг sub-band-ов удваивается (§2.3.3.6).
const SPX_END_LINEAR_CODES: u8 = 3;

/// `spx_begin_subbnd` по `spxbegf` (§2.3.3.5).
pub(crate) const fn spx_begin_subband(begin_code: u8) -> u8 {
    if begin_code < SPX_BEGIN_LINEAR_CODES {
        begin_code + 2
    } else {
        begin_code * 2 - 3
    }
}

/// `spx_end_subbnd` по `spxendf` (§2.3.3.6).
const fn spx_end_subband(end_code: u8) -> u8 {
    if end_code < SPX_END_LINEAR_CODES {
        end_code + 5
    } else {
        end_code * 2 + 3
    }
}

/// `spxstre` … `spxbndstrc`: стратегия SPX блока (в блоке 0 `spxstre = 1`).
pub(super) fn read_spx_strategy(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    state: &mut EnhancedBlockState,
) -> Result<Option<SpxStrategy>, Ac3DecodeError> {
    let overrun = context.base.overrun();
    let strategy_present = context.base.block.get() == 0 || reader.read_flag().map_err(&overrun)?;
    if !strategy_present {
        return Ok(state.spx);
    }
    if !reader.read_flag().map_err(&overrun)? {
        state.leave_spectral_extension();
        return Ok(None);
    }
    let mut in_spx = [false; MAX_FULL_BANDWIDTH_CHANNELS];
    if context.base.mode == AudioCodingMode::Mono {
        in_spx[0] = true;
    } else {
        for flag in in_spx.iter_mut().take(context.base.channel_count) {
            *flag = reader.read_flag().map_err(&overrun)?;
        }
    }
    let copy_start_code = reader.read_u8(2).map_err(&overrun)?;
    let begin_code = reader.read_u8(3).map_err(&overrun)?;
    let end_code = reader.read_u8(3).map_err(&overrun)?;
    let begin_subband = spx_begin_subband(begin_code);
    let end_subband = spx_end_subband(end_code);
    if reader.read_flag().map_err(&overrun)? {
        for slot in state
            .spx_band_structure
            .iter_mut()
            .take(usize::from(end_subband))
            .skip(usize::from(begin_subband) + 1)
        {
            *slot = reader.read_flag().map_err(&overrun)?;
        }
    }
    if begin_subband >= end_subband {
        return Err(context
            .base
            .violation(BitstreamViolation::InvertedSpectralExtensionRange {
                begin_subband,
                end_subband,
            }));
    }
    if copy_start_code >= begin_subband {
        return Err(context
            .base
            .violation(BitstreamViolation::EmptySpectralExtensionCopyRange {
                copy_start_subband: copy_start_code,
                begin_subband,
            }));
    }
    let strategy = SpxStrategy {
        in_spx,
        copy_start_subband: usize::from(copy_start_code),
        begin_code,
        layout: SpxBandLayout::new(
            usize::from(begin_subband),
            usize::from(end_subband),
            &state.spx_band_structure,
        ),
    };
    state.spx = Some(strategy);
    Ok(Some(strategy))
}

/// `spxcoe` … `spxcomant`: координаты SPX каналов (при `spxinu = 1`).
pub(super) fn read_spx_coordinates(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    strategy: &SpxStrategy,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    for channel in 0..context.base.channel_count {
        if !strategy.in_spx[channel] {
            state.first_spx_coordinates[channel] = true;
            state.spx_bands[channel] = None;
            continue;
        }
        let new_coordinates = if state.first_spx_coordinates[channel] {
            state.first_spx_coordinates[channel] = false;
            true
        } else {
            reader.read_flag().map_err(&overrun)?
        };
        if !new_coordinates {
            let reusable =
                state.spx_bands[channel].is_some_and(|stored| stored.layout == strategy.layout);
            if !reusable {
                return Err(context.base.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::SpectralExtensionCoordinates,
                )));
            }
            continue;
        }
        let blend_code = reader.read_u8(5).map_err(&overrun)?;
        let master = reader.read_u8(2).map_err(&overrun)?;
        let mut coordinates = [0.0f32; SPX_SUBBANDS];
        for coordinate in coordinates.iter_mut().take(strategy.layout.band_count()) {
            let exponent = reader.read_u8(4).map_err(&overrun)?;
            let mantissa = reader.read_u8(2).map_err(&overrun)?;
            *coordinate = spx_coordinate(exponent, mantissa, master);
        }
        // Коэффициенты смешения считаются только вместе с новыми координатами (§3.6.4.2.1).
        let (noise_blend, signal_blend) = blending_factors(&strategy.layout, blend_code);
        state.spx_bands[channel] = Some(StoredSpxBands {
            layout: strategy.layout,
            bands: SpxChannelBands {
                coordinates,
                noise_blend,
                signal_blend,
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{spx_begin_subband, spx_end_subband};

    #[test]
    fn subband_codes_follow_sections_2_3_3_5_and_2_3_3_6() {
        let begins: Vec<u8> = (0..8).map(spx_begin_subband).collect();
        assert_eq!(begins, [2, 3, 4, 5, 6, 7, 9, 11]);
        let ends: Vec<u8> = (0..8).map(spx_end_subband).collect();
        assert_eq!(ends, [5, 6, 7, 9, 11, 13, 15, 17]);
    }
}
