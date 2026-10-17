//! Dolby `AudioChannelConfiguration`: 16-битная маска громкоговорителей.
//!
//! Схемы `tag:dolby.com,2014:dash:audio_channel_configuration:2011` и
//! `urn:dolby:dash:audio_channel_configuration:2011` (DASH-IF IOP для AC-3/E-AC-3,
//! ETSI TS 102 366 Annex I) записывают `value` как четыре шестнадцатеричные цифры.
//! Бит 15 (старший) — первая позиция таблицы. Часть битов обозначает ПАРУ
//! громкоговорителей, поэтому число каналов — не просто количество единиц.

/// Схема Dolby, которую использует DASH-IF IOP (`tag:` URI).
pub(crate) const DOLBY_TAG_CHANNEL_CONFIGURATION_SCHEME: &str =
    "tag:dolby.com,2014:dash:audio_channel_configuration:2011";
/// Старая `urn:` запись той же схемы, встречающаяся в реальных MPD.
pub(crate) const DOLBY_URN_CHANNEL_CONFIGURATION_SCHEME: &str =
    "urn:dolby:dash:audio_channel_configuration:2011";

/// Число шестнадцатеричных цифр в `value` (ровно 16 бит маски).
const DOLBY_CHANNEL_MASK_HEX_DIGITS: usize = 4;

/// Биты, обозначающие пару громкоговорителей (левый + правый).
///
/// Нумерация от старшего бита: 5 Lc/Rc, 6 Lrs/Rrs, 9 Lsd/Rsd, 10 Lw/Rw,
/// 11 Vhl/Vhr, 13 Lts/Rts. Остальные биты (L, C, R, Ls, Rs, Cs, Ts, Vhc, LFE2,
/// LFE) — одиночные каналы.
const DOLBY_SPEAKER_PAIR_BITS: u16 = (1 << (15 - 5))
    | (1 << (15 - 6))
    | (1 << (15 - 9))
    | (1 << (15 - 10))
    | (1 << (15 - 11))
    | (1 << (15 - 13));

/// Проверенная ненулевая Dolby-маска громкоговорителей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashDolbyChannelMask(u16);

impl DashDolbyChannelMask {
    /// Разбирает `value` строго из четырёх hex-цифр; пустая маска не доказывает каналы.
    ///
    /// Невалидное значение возвращает `None`: parser сохраняет прежнюю семантику
    /// «неподдерживаемая схема» (Representation отвергается локально, MPD не падает).
    pub fn parse_hex(value: &str) -> Option<Self> {
        if value.len() != DOLBY_CHANNEL_MASK_HEX_DIGITS
            || !value.bytes().all(|digit| digit.is_ascii_hexdigit())
        {
            return None;
        }
        let mask = u16::from_str_radix(value, 16).ok()?;
        (mask != 0).then_some(Self(mask))
    }

    /// Исходная 16-битная маска.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Число каналов: одиночный бит = 1 канал, парный бит = 2 канала.
    pub const fn channel_count(self) -> u16 {
        let paired = self.0 & DOLBY_SPEAKER_PAIR_BITS;
        // count_ones() ≤ 16, сумма ≤ 22 — переполнения u16 нет.
        (self.0.count_ones() + paired.count_ones()) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Типовые раскладки DASH-IF IOP дают ожидаемое число каналов.
    #[test]
    fn standard_layouts_count_single_and_paired_speakers() {
        for (value, channels) in [
            ("A000", 2), // L R
            ("4000", 1), // C
            ("F801", 6), // L C R Ls Rs LFE = 5.1
            ("FA01", 8), // 5.1 + Lrs/Rrs = 7.1
            ("fa01", 8), // регистр hex не важен
        ] {
            let mask = DashDolbyChannelMask::parse_hex(value)
                .unwrap_or_else(|| panic!("{value} must parse"));
            assert_eq!(mask.channel_count(), channels, "mask {value}");
        }
    }

    /// Неполная, лишняя, не-hex и пустая маска не превращаются в число каналов.
    #[test]
    fn malformed_or_empty_mask_is_not_evidence() {
        for value in ["", "F80", "F8010", "+801", "G801", "0000", " F801"] {
            assert_eq!(DashDolbyChannelMask::parse_hex(value), None, "{value:?}");
        }
    }
}
