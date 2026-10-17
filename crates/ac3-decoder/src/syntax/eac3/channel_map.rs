//! Позиции каналов substream-ов E-AC-3: `chanmap` (A/52 Annex E §2.3.1.7–2.3.1.8,
//! Таблица E2.5; ETSI TS 102 366 v1.4.1 Таблица E.1.4) и раскладка по `acmod`.
//!
//! Кодированные каналы зависимого substream-а идут в порядке установленных
//! бит `chanmap`, начиная со «бита 0» (старший бит поля). Бит пары (Lc/Rc,
//! Lrs/Rrs, …) описывает два соседних кодированных канала. Без `chanmap`
//! позиции задаёт `acmod`/`lfeon` (Таблица 5.8, LFE последним) — и такой
//! зависимый substream заменяет одноимённые каналы независимого (§3.8.2).

use crate::output::{ChannelPosition, ac3_channel_positions};
use crate::syntax::bsi_values::AudioCodingMode;

/// Число позиций в Таблице E2.5.
const CHANNEL_MAP_LOCATIONS: usize = 16;

/// Таблица E2.5: позиции по номеру бита (бит 0 — старший бит `chanmap`).
const LOCATIONS: [&[ChannelPosition]; CHANNEL_MAP_LOCATIONS] = {
    use ChannelPosition::{
        Center, CenterSurround, Left, LeftCenter, LeftRearSurround, LeftSurround,
        LeftSurroundDirect, LeftTopSurround, LeftWide, LowFrequencyEffects, LowFrequencyEffects2,
        Right, RightCenter, RightRearSurround, RightSurround, RightSurroundDirect,
        RightTopSurround, RightWide, TopSurround, VerticalHeightCenter, VerticalHeightLeft,
        VerticalHeightRight,
    };
    [
        &[Left],
        &[Center],
        &[Right],
        &[LeftSurround],
        &[RightSurround],
        &[LeftCenter, RightCenter],
        &[LeftRearSurround, RightRearSurround],
        &[CenterSurround],
        &[TopSurround],
        &[LeftSurroundDirect, RightSurroundDirect],
        &[LeftWide, RightWide],
        &[VerticalHeightLeft, VerticalHeightRight],
        &[VerticalHeightCenter],
        &[LeftTopSurround, RightTopSurround],
        &[LowFrequencyEffects2],
        &[LowFrequencyEffects],
    ]
};

/// 16-битный `chanmap` зависимого substream-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CustomChannelMap {
    bits: u16,
}

impl CustomChannelMap {
    pub(crate) const fn from_bits(bits: u16) -> Self {
        Self { bits }
    }

    /// Исходное поле (бит 0 Таблицы E2.5 — старший бит).
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.bits
    }

    /// Позиции кодированных каналов в порядке потока.
    #[must_use]
    pub fn positions(self) -> Vec<ChannelPosition> {
        let mut positions = Vec::new();
        self.extend_positions(&mut positions);
        positions
    }

    /// Дописывает позиции в `positions` (без новых аллокаций при повторном использовании).
    pub(crate) fn extend_positions(self, positions: &mut Vec<ChannelPosition>) {
        for (location, channels) in LOCATIONS.iter().enumerate() {
            let mask = 1u16 << (CHANNEL_MAP_LOCATIONS - 1 - location);
            if self.bits & mask != 0 {
                positions.extend_from_slice(channels);
            }
        }
    }
}

/// Позиции кодированных каналов substream-а в порядке потока: fbw по
/// Таблице 5.8, затем LFE — или по `chanmap`, если он передан.
pub(crate) fn substream_positions(
    mode: AudioCodingMode,
    lfe_on: bool,
    channel_map: Option<CustomChannelMap>,
    positions: &mut Vec<ChannelPosition>,
) {
    positions.clear();
    match channel_map {
        Some(map) => map.extend_positions(positions),
        None => positions.extend(ac3_channel_positions(mode, lfe_on)),
    }
}

#[cfg(test)]
mod tests {
    use super::{CustomChannelMap, substream_positions};
    use crate::output::ChannelPosition::{
        Center, Left, LeftRearSurround, LeftSurround, LowFrequencyEffects, RightRearSurround,
        RightSurround, VerticalHeightCenter,
    };
    use crate::syntax::bsi_values::AudioCodingMode;

    /// Примеры из §2.3.1.8: биты 0, 3, 4 → L, Ls, Rs; биты 3, 4, 6 → Ls, Rs, Lrs, Rrs.
    #[test]
    fn spec_examples_map_bits_to_coded_channel_order() {
        let first = CustomChannelMap::from_bits(0b1001_1000_0000_0000);
        assert_eq!(first.positions(), [Left, LeftSurround, RightSurround]);
        let second = CustomChannelMap::from_bits(0b0001_1010_0000_0000);
        assert_eq!(
            second.positions(),
            [
                LeftSurround,
                RightSurround,
                LeftRearSurround,
                RightRearSurround
            ]
        );
        // Vhc (бит 12) и LFE (бит 15, младший бит поля).
        let third = CustomChannelMap::from_bits(0b0000_0000_0000_1001);
        assert_eq!(
            third.positions(),
            [VerticalHeightCenter, LowFrequencyEffects]
        );
    }

    #[test]
    fn without_channel_map_positions_follow_acmod_with_lfe_last() {
        let mut positions = vec![Left; 9];
        substream_positions(AudioCodingMode::Mono, true, None, &mut positions);
        assert_eq!(positions, [Center, LowFrequencyEffects]);
    }
}
