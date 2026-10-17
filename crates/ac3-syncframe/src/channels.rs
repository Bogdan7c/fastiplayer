//! Расположение каналов в терминах 16-битной карты `chanmap` E-AC-3.

/// Набор позиций каналов в нотации `chanmap` (A/52 таблица E2.5).
///
/// Бит 15 (старший) — позиция 0 (L), бит 0 — позиция 15 (LFE). Некоторые
/// позиции обозначают пару каналов (например, Lrs/Rrs), поэтому число каналов
/// не равно числу установленных битов — его считает [`ChannelLocations::channel_count`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ChannelLocations(u16);

/// Номера позиций chanmap, которые обозначают пару каналов:
/// 5 Lc/Rc, 6 Lrs/Rrs, 9 Lsd/Rsd, 10 Lw/Rw, 11 Lvh/Rvh, 13 Lts/Rts.
const PAIR_LOCATION_INDICES: [u8; 6] = [5, 6, 9, 10, 11, 13];

/// Позиции chanmap, которыми описываются раскладки `acmod`.
mod location {
    pub(super) const LEFT: u8 = 0;
    pub(super) const CENTER: u8 = 1;
    pub(super) const RIGHT: u8 = 2;
    pub(super) const LEFT_SURROUND: u8 = 3;
    pub(super) const RIGHT_SURROUND: u8 = 4;
    pub(super) const CENTER_SURROUND: u8 = 7;
    pub(super) const LOW_FREQUENCY_EFFECTS: u8 = 15;
}

/// Сколько позиций содержит 16-битная карта.
const LOCATION_COUNT: u8 = 16;

impl ChannelLocations {
    /// Карта из «сырого» поля `chanmap` dependent substream-а.
    #[must_use]
    pub const fn from_chanmap(chanmap: u16) -> Self {
        Self(chanmap)
    }

    /// Исходное 16-битное значение в нотации `chanmap`.
    #[must_use]
    pub const fn as_chanmap(self) -> u16 {
        self.0
    }

    /// Карта для стандартной раскладки `acmod` (+LFE), как её описывает A/52.
    ///
    /// Двойное моно (`acmod == 0`, «1+1») не имеет своих позиций в chanmap;
    /// оба независимых канала кладутся в L и R, чтобы счётчик дал 2.
    pub(crate) fn from_coding_mode(full_bandwidth: &[u8], low_frequency_effects: bool) -> Self {
        let mut locations = Self::default();
        for &index in full_bandwidth {
            locations = locations.with_location(index);
        }
        if low_frequency_effects {
            locations = locations.with_location(location::LOW_FREQUENCY_EFFECTS);
        }
        locations
    }

    /// Объединение двух наборов позиций (независимый substream + dependent).
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Число реальных каналов с учётом парных позиций.
    #[must_use]
    pub fn channel_count(self) -> u32 {
        (0..LOCATION_COUNT)
            .filter(|&index| self.contains(index))
            .map(|index| {
                if PAIR_LOCATION_INDICES.contains(&index) {
                    2
                } else {
                    1
                }
            })
            .sum()
    }

    const fn contains(self, index: u8) -> bool {
        self.0 & Self::bit(index) != 0
    }

    const fn with_location(self, index: u8) -> Self {
        Self(self.0 | Self::bit(index))
    }

    /// Позиция 0 — старший бит, как в A/52 E.1.3.1.8.
    const fn bit(index: u8) -> u16 {
        1 << (LOCATION_COUNT - 1 - index)
    }
}

/// Позиции full-bandwidth каналов для каждого `acmod` (A/52 таблица 5.8).
pub(crate) const fn coding_mode_locations(acmod: u8) -> &'static [u8] {
    use location::{CENTER, CENTER_SURROUND, LEFT, LEFT_SURROUND, RIGHT, RIGHT_SURROUND};
    match acmod {
        // 1+1 dual mono: два независимых канала (Ch1, Ch2).
        0 => &[LEFT, RIGHT],
        1 => &[CENTER],
        2 => &[LEFT, RIGHT],
        3 => &[LEFT, CENTER, RIGHT],
        4 => &[LEFT, RIGHT, CENTER_SURROUND],
        5 => &[LEFT, CENTER, RIGHT, CENTER_SURROUND],
        6 => &[LEFT, RIGHT, LEFT_SURROUND, RIGHT_SURROUND],
        _ => &[LEFT, CENTER, RIGHT, LEFT_SURROUND, RIGHT_SURROUND],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_locations_count_as_two_channels() {
        // L, R, Lrs/Rrs (пара), LFE.
        let locations = ChannelLocations::from_chanmap(0b1010_0010_0000_0001);
        assert_eq!(locations.channel_count(), 5);
    }

    #[test]
    fn five_one_plus_back_pair_is_seven_one() {
        let five_one = ChannelLocations::from_coding_mode(coding_mode_locations(7), true);
        assert_eq!(five_one.channel_count(), 6);
        // Dependent substream добавляет Lrs/Rrs (позиция 6).
        let back_pair = ChannelLocations::from_chanmap(1 << (15 - 6));
        assert_eq!(five_one.union(back_pair).channel_count(), 8);
    }

    #[test]
    fn every_coding_mode_has_expected_full_bandwidth_count() {
        let expected = [2, 1, 2, 3, 3, 4, 4, 5];
        for (acmod, channels) in expected.into_iter().enumerate() {
            let acmod = u8::try_from(acmod).expect("acmod fits u8");
            let locations = ChannelLocations::from_coding_mode(coding_mode_locations(acmod), false);
            assert_eq!(locations.channel_count(), channels, "acmod {acmod}");
        }
    }
}
