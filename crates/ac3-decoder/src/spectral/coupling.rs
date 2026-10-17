//! Канальное coupling: координаты и восстановление каналов (A/52 §7.4).
//!
//! Выше частоты `cplbegf` каналы, участвующие в coupling, передаются одним
//! общим coupling каналом и набором координат на каждую полосу. Декодер
//! восстанавливает коэффициенты каждого канала как
//! `cplmant · 2^−cplexp · cplco[ch][sbnd] · 8` (§7.4.3), а в режиме 2/0 ещё и
//! инвертирует правый канал в полосах с `phsflg = 1` (§7.4.1).

use super::dither::{ChannelDither, DitherGenerator};
use super::{COEFFICIENTS_PER_BLOCK, exponent_scale};

/// Первый бин coupling sub-band 0 (Таблица 7.24).
pub(crate) const COUPLING_FIRST_BIN: usize = 37;
/// Ширина coupling sub-band в бинах (Таблица 7.24).
pub(crate) const COUPLING_SUBBAND_WIDTH: usize = 12;
/// Число coupling sub-band-ов (Таблица 7.24: 0–17).
pub(crate) const MAX_COUPLING_SUBBANDS: usize = 18;
/// Множитель восстановления из псевдокода §7.4.3.
const DECOUPLING_GAIN: f32 = 8.0;

/// Первый бин sub-band-а `subband` (Таблица 7.24: `37 + 12·sbnd`).
pub(crate) const fn subband_first_bin(subband: usize) -> usize {
    COUPLING_FIRST_BIN + COUPLING_SUBBAND_WIDTH * subband
}

/// Координата coupling одной полосы (§7.4.3).
///
/// `cplcomant / 16` при `cplcoexp == 15`, иначе `(cplcomant + 16) / 32`;
/// затем сдвиг вправо на `cplcoexp + 3·mstrcplco` (до 24 разрядов).
pub(crate) fn coupling_coordinate(exponent: u8, mantissa: u8, master_exponent: u8) -> f32 {
    /// Экспонента, при которой мантисса передаётся без скрытой единицы.
    const DENORMAL_EXPONENT: u8 = 15;
    let base = if exponent == DENORMAL_EXPONENT {
        f32::from(mantissa) / 16.0
    } else {
        f32::from(mantissa + 16) / 32.0
    };
    let shift = exponent.saturating_add(master_exponent.saturating_mul(3));
    base * exponent_scale(shift)
}

/// Что известно о coupling channel-е в текущем блоке.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CouplingChannelView<'a> {
    /// Деквантованные мантиссы coupling канала (без экспоненты), по бинам.
    pub(crate) mantissas: &'a [f32; COEFFICIENTS_PER_BLOCK],
    /// Указатели bap coupling канала: bap = 0 → мантисса заменяется dither-ом
    /// канала-получателя («after the individual channels are extracted», §7.3.4).
    pub(crate) bap: &'a [u8; COEFFICIENTS_PER_BLOCK],
    /// Экспоненты coupling канала.
    pub(crate) exponents: &'a [u8; COEFFICIENTS_PER_BLOCK],
    /// Первый coupling sub-band (`cplbegf`).
    pub(crate) begin_subband: usize,
    /// Sub-band за последним (`cplendf + 3`).
    pub(crate) end_subband: usize,
}

/// Координаты и фазовые флаги одного канала, уже разложенные по sub-band-ам.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelCouplingGains<'a> {
    pub(crate) coordinates: &'a [f32; MAX_COUPLING_SUBBANDS],
    /// Инвертировать ли sub-band (только правый канал 2/0 с `phsflginu`).
    pub(crate) phase_inverted: &'a [bool; MAX_COUPLING_SUBBANDS],
    pub(crate) dither: ChannelDither,
}

/// Восстанавливает coupled часть спектра канала в `coefficients`.
pub(crate) fn decouple_channel(
    coupling: &CouplingChannelView<'_>,
    gains: &ChannelCouplingGains<'_>,
    dither: &mut DitherGenerator,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) {
    let end_subband = coupling.end_subband.min(MAX_COUPLING_SUBBANDS);
    for subband in coupling.begin_subband..end_subband {
        let gain = gains.coordinates[subband] * DECOUPLING_GAIN;
        let sign = if gains.phase_inverted[subband] {
            -1.0
        } else {
            1.0
        };
        let first_bin = subband_first_bin(subband);
        for bin in first_bin..first_bin + COUPLING_SUBBAND_WIDTH {
            let (Some(&bap), Some(&mantissa), Some(&exponent), Some(slot)) = (
                coupling.bap.get(bin),
                coupling.mantissas.get(bin),
                coupling.exponents.get(bin),
                coefficients.get_mut(bin),
            ) else {
                continue;
            };
            let mantissa = if bap == 0 {
                dither.zero_bit_mantissa(gains.dither)
            } else {
                mantissa
            };
            *slot = sign * mantissa * exponent_scale(exponent) * gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COEFFICIENTS_PER_BLOCK, ChannelCouplingGains, CouplingChannelView, MAX_COUPLING_SUBBANDS,
        coupling_coordinate, decouple_channel, subband_first_bin,
    };
    use crate::spectral::dither::{ChannelDither, DitherGenerator, DitherMode};

    #[test]
    fn coordinate_format_follows_section_7_4_3() {
        // exp < 15: скрытая единица, (mant + 16) / 32 · 2^−exp.
        assert_eq!(coupling_coordinate(0, 0, 0), 0.5);
        assert_eq!(coupling_coordinate(0, 15, 0), 31.0 / 32.0);
        assert_eq!(coupling_coordinate(2, 8, 0), 24.0 / 32.0 / 4.0);
        // exp == 15: mant / 16 · 2^−15.
        assert_eq!(coupling_coordinate(15, 8, 0), 0.5 / 32_768.0);
        // mstrcplco добавляет 3 разряда на единицу (Таблица 5.14).
        assert_eq!(coupling_coordinate(1, 0, 2), 0.5 / 2.0 / 64.0);
        assert_eq!(subband_first_bin(17), 241);
    }

    #[test]
    fn decoupling_scales_by_coordinate_and_respects_phase_and_range() {
        let mut mantissas = [0.0f32; COEFFICIENTS_PER_BLOCK];
        let mut bap = [0u8; COEFFICIENTS_PER_BLOCK];
        let mut exponents = [0u8; COEFFICIENTS_PER_BLOCK];
        for bin in 61..85 {
            mantissas[bin] = 0.5;
            bap[bin] = 3;
            exponents[bin] = 1;
        }
        let view = CouplingChannelView {
            mantissas: &mantissas,
            bap: &bap,
            exponents: &exponents,
            begin_subband: 2,
            end_subband: 4,
        };
        let mut coordinates = [0.0f32; MAX_COUPLING_SUBBANDS];
        coordinates[2] = 0.25;
        coordinates[3] = 0.125;
        let mut phase = [false; MAX_COUPLING_SUBBANDS];
        phase[3] = true;
        let gains = ChannelCouplingGains {
            coordinates: &coordinates,
            phase_inverted: &phase,
            dither: ChannelDither::Off,
        };
        let mut coefficients = [9.0f32; COEFFICIENTS_PER_BLOCK];
        decouple_channel(
            &view,
            &gains,
            &mut DitherGenerator::new(DitherMode::default()),
            &mut coefficients,
        );
        // 0.5 · 2^−1 · 0.25 · 8 = 0.5; второй sub-band — с инверсией.
        assert!(coefficients[61..73].iter().all(|&value| value == 0.5));
        assert!(coefficients[73..85].iter().all(|&value| value == -0.25));
        assert_eq!(coefficients[60], 9.0);
        assert_eq!(coefficients[85], 9.0);
    }

    #[test]
    fn zero_bit_coupling_bins_take_per_channel_dither() {
        let mantissas = [0.0f32; COEFFICIENTS_PER_BLOCK];
        let bap = [0u8; COEFFICIENTS_PER_BLOCK];
        let exponents = [0u8; COEFFICIENTS_PER_BLOCK];
        let view = CouplingChannelView {
            mantissas: &mantissas,
            bap: &bap,
            exponents: &exponents,
            begin_subband: 0,
            end_subband: 1,
        };
        let coordinates = [1.0f32; MAX_COUPLING_SUBBANDS];
        let phase = [false; MAX_COUPLING_SUBBANDS];
        let mut generator = DitherGenerator::new(DitherMode::default());
        let mut requested = [0.0f32; COEFFICIENTS_PER_BLOCK];
        let gains = ChannelCouplingGains {
            coordinates: &coordinates,
            phase_inverted: &phase,
            dither: ChannelDither::Requested,
        };
        decouple_channel(&view, &gains, &mut generator, &mut requested);
        assert!(
            requested[37..49]
                .iter()
                .all(|&value| value != 0.0 && value.abs() <= 0.707 * 8.0)
        );
        let mut silent = [0.0f32; COEFFICIENTS_PER_BLOCK];
        let off = ChannelCouplingGains {
            dither: ChannelDither::Off,
            ..gains
        };
        decouple_channel(&view, &off, &mut generator, &mut silent);
        assert!(silent.iter().all(|&value| value == 0.0));
    }
}
