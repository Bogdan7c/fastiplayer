//! Нейтральное описание порядка каналов decoded PCM.
//!
//! Модуль намеренно не использует типы Symphonia или CPAL. Codec adapter
//! переводит backend layout в этот контракт, а concrete output решает, как
//! преобразовать его в layout выбранного устройства.

use std::fmt;

use thiserror::Error;

/// Физическая позиция одного канала в interleaved PCM frame.
///
/// Числовой порядок является внутренним каноническим порядком audio-core:
/// один и тот же layout всегда даёт одинаковый lane index независимо от codec-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AudioChannelPosition {
    /// Передний левый канал.
    FrontLeft = 0,
    /// Передний правый канал.
    FrontRight = 1,
    /// Передний центральный канал; также используется для mono.
    FrontCenter = 2,
    /// Первый low-frequency-effects канал.
    LowFrequencyEffects = 3,
    /// Задний левый surround-канал.
    RearLeft = 4,
    /// Задний правый surround-канал.
    RearRight = 5,
    /// Передний канал слева от центра.
    FrontLeftOfCenter = 6,
    /// Передний канал справа от центра.
    FrontRightOfCenter = 7,
    /// Задний центральный surround-канал.
    RearCenter = 8,
    /// Боковой левый surround-канал.
    SideLeft = 9,
    /// Боковой правый surround-канал.
    SideRight = 10,
    /// Верхний центральный канал.
    TopCenter = 11,
    /// Верхний передний левый канал.
    TopFrontLeft = 12,
    /// Верхний передний центральный канал.
    TopFrontCenter = 13,
    /// Верхний передний правый канал.
    TopFrontRight = 14,
    /// Верхний задний левый канал.
    TopRearLeft = 15,
    /// Верхний задний центральный канал.
    TopRearCenter = 16,
    /// Верхний задний правый канал.
    TopRearRight = 17,
    /// Второй low-frequency-effects канал.
    LowFrequencyEffects2 = 18,
    /// Верхний боковой левый канал.
    TopSideLeft = 19,
    /// Верхний боковой правый канал.
    TopSideRight = 20,
    /// Нижний передний центральный канал.
    BottomFrontCenter = 21,
    /// Нижний передний левый канал.
    BottomFrontLeft = 22,
    /// Нижний передний правый канал.
    BottomFrontRight = 23,
    /// Широкий передний левый канал.
    FrontLeftWide = 24,
    /// Широкий передний правый канал.
    FrontRightWide = 25,
}

impl AudioChannelPosition {
    /// Возвращает бит neutral position mask-а.
    const fn mask(self) -> u64 {
        1_u64 << self as u8
    }

    /// Восстанавливает позицию из одного канонического bit index.
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::FrontLeft,
            1 => Self::FrontRight,
            2 => Self::FrontCenter,
            3 => Self::LowFrequencyEffects,
            4 => Self::RearLeft,
            5 => Self::RearRight,
            6 => Self::FrontLeftOfCenter,
            7 => Self::FrontRightOfCenter,
            8 => Self::RearCenter,
            9 => Self::SideLeft,
            10 => Self::SideRight,
            11 => Self::TopCenter,
            12 => Self::TopFrontLeft,
            13 => Self::TopFrontCenter,
            14 => Self::TopFrontRight,
            15 => Self::TopRearLeft,
            16 => Self::TopRearCenter,
            17 => Self::TopRearRight,
            18 => Self::LowFrequencyEffects2,
            19 => Self::TopSideLeft,
            20 => Self::TopSideRight,
            21 => Self::BottomFrontCenter,
            22 => Self::BottomFrontLeft,
            23 => Self::BottomFrontRight,
            24 => Self::FrontLeftWide,
            25 => Self::FrontRightWide,
            _ => return None,
        })
    }
}

impl fmt::Display for AudioChannelPosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Внутреннее компактное представление layout-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AudioChannelLayoutRepresentation {
    /// Позиционные каналы в каноническом порядке младших битов.
    Positioned(u64),
    /// Каналы без известной физической позиции.
    Discrete(u16),
}

/// Нейтральный layout interleaved PCM frame.
///
/// Для positional layout lane order всегда канонический: позиции идут по
/// возрастанию discriminant [`AudioChannelPosition`]. `Discrete` сохраняет лишь
/// независимые lane indices, поэтому output не имеет права угадывать surround
/// semantics при изменении числа каналов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioChannelLayout {
    representation: AudioChannelLayoutRepresentation,
}

impl AudioChannelLayout {
    /// Создаёт mono layout с front-center каналом.
    #[must_use]
    pub const fn mono() -> Self {
        Self {
            representation: AudioChannelLayoutRepresentation::Positioned(
                AudioChannelPosition::FrontCenter.mask(),
            ),
        }
    }

    /// Создаёт стандартный stereo layout `front-left, front-right`.
    #[must_use]
    pub const fn stereo() -> Self {
        Self {
            representation: AudioChannelLayoutRepresentation::Positioned(
                AudioChannelPosition::FrontLeft.mask() | AudioChannelPosition::FrontRight.mask(),
            ),
        }
    }

    /// Создаёт canonical 5.1 layout с rear-left/rear-right surround каналами.
    #[must_use]
    pub const fn surround_5_1() -> Self {
        Self {
            representation: AudioChannelLayoutRepresentation::Positioned(
                AudioChannelPosition::FrontLeft.mask()
                    | AudioChannelPosition::FrontRight.mask()
                    | AudioChannelPosition::FrontCenter.mask()
                    | AudioChannelPosition::LowFrequencyEffects.mask()
                    | AudioChannelPosition::RearLeft.mask()
                    | AudioChannelPosition::RearRight.mask(),
            ),
        }
    }

    /// Создаёт canonical 5.1 layout с side-left/side-right surround каналами.
    #[must_use]
    pub const fn surround_5_1_side() -> Self {
        Self {
            representation: AudioChannelLayoutRepresentation::Positioned(
                AudioChannelPosition::FrontLeft.mask()
                    | AudioChannelPosition::FrontRight.mask()
                    | AudioChannelPosition::FrontCenter.mask()
                    | AudioChannelPosition::LowFrequencyEffects.mask()
                    | AudioChannelPosition::SideLeft.mask()
                    | AudioChannelPosition::SideRight.mask(),
            ),
        }
    }

    /// Создаёт positional layout из набора позиций.
    ///
    /// Порядок аргумента не переносится в PCM: результат всегда использует
    /// канонический порядок. Это не позволяет codec adapter-у случайно протащить
    /// codec-specific lane order через нейтральную границу.
    pub fn positioned(positions: &[AudioChannelPosition]) -> Result<Self, AudioChannelLayoutError> {
        if positions.is_empty() {
            return Err(AudioChannelLayoutError::EmptyPositionedLayout);
        }

        let mut position_mask = 0_u64;
        for position in positions {
            let position_bit = position.mask();
            if position_mask & position_bit != 0 {
                return Err(AudioChannelLayoutError::DuplicatePosition {
                    position: *position,
                });
            }
            position_mask |= position_bit;
        }

        Ok(Self {
            representation: AudioChannelLayoutRepresentation::Positioned(position_mask),
        })
    }

    /// Создаёт layout с независимыми каналами без speaker positions.
    pub fn discrete(channel_count: u32) -> Result<Self, AudioChannelLayoutError> {
        let channel_count = u16::try_from(channel_count)
            .ok()
            .filter(|channel_count| *channel_count > 0)
            .ok_or(AudioChannelLayoutError::InvalidDiscreteChannelCount { channel_count })?;

        Ok(Self {
            representation: AudioChannelLayoutRepresentation::Discrete(channel_count),
        })
    }

    /// Создаёт лучший layout только из container channel count.
    ///
    /// Mono/stereo имеют однозначную общепринятую семантику. Multichannel count
    /// без layout остаётся `Discrete`: додумывать 5.1 порядок здесь опасно.
    pub fn from_channel_count(channel_count: u32) -> Result<Self, AudioChannelLayoutError> {
        match channel_count {
            1 => Ok(Self::mono()),
            2 => Ok(Self::stereo()),
            _ => Self::discrete(channel_count),
        }
    }

    /// Возвращает количество scalar lanes в одном interleaved frame.
    #[must_use]
    pub const fn channel_count(self) -> u32 {
        match self.representation {
            AudioChannelLayoutRepresentation::Positioned(position_mask) => {
                position_mask.count_ones()
            }
            AudioChannelLayoutRepresentation::Discrete(channel_count) => channel_count as u32,
        }
    }

    /// Возвращает позицию по каноническому lane index либо `None` для discrete layout-а.
    #[must_use]
    pub fn position_at(self, lane_index: usize) -> Option<AudioChannelPosition> {
        let AudioChannelLayoutRepresentation::Positioned(mut position_mask) = self.representation
        else {
            return None;
        };

        for _ in 0..lane_index {
            if position_mask == 0 {
                return None;
            }
            position_mask &= position_mask - 1;
        }

        let bit_index = u8::try_from(position_mask.trailing_zeros()).ok()?;
        AudioChannelPosition::from_index(bit_index)
    }

    /// Проверяет наличие физической позиции в positional layout-е.
    #[must_use]
    pub const fn contains(self, position: AudioChannelPosition) -> bool {
        match self.representation {
            AudioChannelLayoutRepresentation::Positioned(position_mask) => {
                position_mask & position.mask() != 0
            }
            AudioChannelLayoutRepresentation::Discrete(_) => false,
        }
    }

    /// Сообщает, известны ли speaker positions всех каналов.
    #[must_use]
    pub const fn is_positioned(self) -> bool {
        matches!(
            self.representation,
            AudioChannelLayoutRepresentation::Positioned(_)
        )
    }
}

impl fmt::Display for AudioChannelLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.representation {
            AudioChannelLayoutRepresentation::Discrete(channel_count) => {
                write!(formatter, "discrete({channel_count})")
            }
            AudioChannelLayoutRepresentation::Positioned(_) => {
                write!(formatter, "positioned[")?;
                for lane_index in 0..self.channel_count() as usize {
                    if lane_index > 0 {
                        write!(formatter, ",")?;
                    }
                    let position = self
                        .position_at(lane_index)
                        .expect("validated positional layout contains every canonical lane");
                    write!(formatter, "{position}")?;
                }
                write!(formatter, "]")
            }
        }
    }
}

/// Ошибка построения neutral channel layout-а.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum AudioChannelLayoutError {
    /// Positional layout не может быть пустым.
    #[error("positioned audio channel layout cannot be empty")]
    EmptyPositionedLayout,

    /// Одна speaker position не может занимать две interleaved lanes.
    #[error("audio channel position {position} is duplicated")]
    DuplicatePosition {
        /// Повторившаяся позиция.
        position: AudioChannelPosition,
    },

    /// Discrete count обязан помещаться в `u16` и быть ненулевым.
    #[error("invalid discrete audio channel count: {channel_count}")]
    InvalidDiscreteChannelCount {
        /// Исходное значение, отвергнутое boundary.
        channel_count: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positioned_layout_uses_canonical_lane_order() {
        let layout = AudioChannelLayout::positioned(&[
            AudioChannelPosition::RearRight,
            AudioChannelPosition::FrontCenter,
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::LowFrequencyEffects,
            AudioChannelPosition::FrontRight,
            AudioChannelPosition::RearLeft,
        ])
        .unwrap();

        assert_eq!(layout.channel_count(), 6);
        assert_eq!(layout.position_at(0), Some(AudioChannelPosition::FrontLeft));
        assert_eq!(
            layout.position_at(1),
            Some(AudioChannelPosition::FrontRight)
        );
        assert_eq!(
            layout.position_at(2),
            Some(AudioChannelPosition::FrontCenter)
        );
        assert_eq!(
            layout.position_at(3),
            Some(AudioChannelPosition::LowFrequencyEffects)
        );
        assert_eq!(layout.position_at(4), Some(AudioChannelPosition::RearLeft));
        assert_eq!(layout.position_at(5), Some(AudioChannelPosition::RearRight));
        assert_eq!(layout.position_at(6), None);
    }

    #[test]
    fn discrete_layout_never_invents_speaker_positions() {
        let layout = AudioChannelLayout::from_channel_count(6).unwrap();

        assert_eq!(layout.channel_count(), 6);
        assert!(!layout.is_positioned());
        assert_eq!(layout.position_at(0), None);
    }

    #[test]
    fn invalid_layouts_are_rejected_with_typed_errors() {
        assert_eq!(
            AudioChannelLayout::positioned(&[]),
            Err(AudioChannelLayoutError::EmptyPositionedLayout)
        );
        assert_eq!(
            AudioChannelLayout::positioned(&[
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontLeft,
            ]),
            Err(AudioChannelLayoutError::DuplicatePosition {
                position: AudioChannelPosition::FrontLeft,
            })
        );
        assert_eq!(
            AudioChannelLayout::discrete(0),
            Err(AudioChannelLayoutError::InvalidDiscreteChannelCount { channel_count: 0 })
        );
    }
}
