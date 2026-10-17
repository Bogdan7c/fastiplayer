//! Перевод каналов AC-3 / E-AC-3 в нейтральный canonical layout.
//!
//! `ac3-decoder` отдаёт планарный PCM в порядке декодирования (A/52 Т. 5.8,
//! LFE последним; зависимые substream-ы E-AC-3 — Т. E2.5) и typed позицию
//! [`ChannelPosition`] каждого канала. Neutral boundary требует interleaved PCM
//! в canonical порядке [`AudioChannelLayout`] (по возрастанию
//! [`AudioChannelPosition`]). Модуль:
//! 1. сопоставляет позиции декодера нейтральным ([`NeutralLayout::for_decoder_channels`]);
//! 2. переставляет и перемежает каналы ([`NeutralLayout::interleave`]);
//! 3. кэширует результат для одинаковой раскладки подряд ([`LayoutConverter`]),
//!    чтобы не пересчитывать перестановку на каждый кадр.
//!
//! Codec-specific порядок каналов наружу не выходит.

use ac3_decoder::{ChannelPosition, DecodedAudio};
use audio_core::{AudioChannelLayout, AudioChannelPosition};

use super::Ac3DecodeError;

/// Запасные нейтральные позиции для `Lsd`/`Rsd` (surround direct), по приоритету.
///
/// У neutral boundary нет отдельной позиции «surround direct». Динамики
/// `Lsd/Rsd` стоят сбоку от слушателя, поэтому сначала занимаем side-пару. Если
/// её уже занимают `Ls/Rs` (они в этом проекте — side, как `5.1(side)` FFmpeg),
/// берём rear-пару: production `ChannelMixer` сводит side и rear в stereo с
/// одинаковым весом −3 дБ, так что звук не теряется и не смещается вперёд.
/// Если заняты обе пары (Ls/Rs + Lrs/Rrs + Lsd/Rsd — 11+ каналов), раскладка
/// считается неконвертируемой ([`UnconvertibleLayout`]).
const LEFT_SURROUND_DIRECT_FALLBACKS: [AudioChannelPosition; 2] = [
    AudioChannelPosition::SideLeft,
    AudioChannelPosition::RearLeft,
];
/// Правая пара к [`LEFT_SURROUND_DIRECT_FALLBACKS`].
const RIGHT_SURROUND_DIRECT_FALLBACKS: [AudioChannelPosition; 2] = [
    AudioChannelPosition::SideRight,
    AudioChannelPosition::RearRight,
];

/// Сопоставляет позицию с однозначным нейтральным эквивалентом.
///
/// Соглашение как у FFmpeg: `Ls/Rs` базового AC-3 — side-пара (FFmpeg пишет 5.1
/// AC-3 как `5.1(side)`), одиночный `S`/`Cs` — задний центр, `Lrs/Rrs` — задняя
/// пара 7.1. `None` — у позиции нет фиксированного эквивалента, её место
/// выбирается по занятости ([`surround_direct_fallbacks`]).
fn fixed_neutral_position(position: ChannelPosition) -> Option<AudioChannelPosition> {
    Some(match position {
        ChannelPosition::Left => AudioChannelPosition::FrontLeft,
        ChannelPosition::Right => AudioChannelPosition::FrontRight,
        ChannelPosition::Center => AudioChannelPosition::FrontCenter,
        ChannelPosition::LowFrequencyEffects => AudioChannelPosition::LowFrequencyEffects,
        ChannelPosition::LowFrequencyEffects2 => AudioChannelPosition::LowFrequencyEffects2,
        ChannelPosition::LeftSurround => AudioChannelPosition::SideLeft,
        ChannelPosition::RightSurround => AudioChannelPosition::SideRight,
        ChannelPosition::CenterSurround => AudioChannelPosition::RearCenter,
        ChannelPosition::LeftRearSurround => AudioChannelPosition::RearLeft,
        ChannelPosition::RightRearSurround => AudioChannelPosition::RearRight,
        ChannelPosition::LeftCenter => AudioChannelPosition::FrontLeftOfCenter,
        ChannelPosition::RightCenter => AudioChannelPosition::FrontRightOfCenter,
        ChannelPosition::LeftWide => AudioChannelPosition::FrontLeftWide,
        ChannelPosition::RightWide => AudioChannelPosition::FrontRightWide,
        ChannelPosition::TopSurround => AudioChannelPosition::TopCenter,
        ChannelPosition::VerticalHeightLeft => AudioChannelPosition::TopFrontLeft,
        ChannelPosition::VerticalHeightRight => AudioChannelPosition::TopFrontRight,
        ChannelPosition::VerticalHeightCenter => AudioChannelPosition::TopFrontCenter,
        ChannelPosition::LeftTopSurround => AudioChannelPosition::TopSideLeft,
        ChannelPosition::RightTopSurround => AudioChannelPosition::TopSideRight,
        ChannelPosition::LeftSurroundDirect | ChannelPosition::RightSurroundDirect => return None,
    })
}

/// Кандидаты для позиции без фиксированного эквивалента (сейчас только `Lsd/Rsd`).
fn surround_direct_fallbacks(position: ChannelPosition) -> &'static [AudioChannelPosition] {
    match position {
        ChannelPosition::LeftSurroundDirect => &LEFT_SURROUND_DIRECT_FALLBACKS,
        ChannelPosition::RightSurroundDirect => &RIGHT_SURROUND_DIRECT_FALLBACKS,
        _ => &[],
    }
}

/// Раскладка программы, для которой нет нейтрального представления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnconvertibleLayout {
    /// Позиции каналов декодера в порядке декодирования.
    pub(super) decoder_channels: Vec<ChannelPosition>,
}

/// Готовая перестановка: canonical layout и источник каждого canonical lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NeutralLayout {
    /// Layout, который получает neutral boundary.
    layout: AudioChannelLayout,
    /// `decoder_channel_for_lane[i]` — индекс канала декодера для canonical lane `i`.
    decoder_channel_for_lane: Vec<usize>,
}

impl NeutralLayout {
    /// Строит перестановку для позиций каналов декодера.
    ///
    /// Сначала раскладываются позиции с фиксированным эквивалентом, затем —
    /// `Lsd/Rsd` на первое свободное место своей цепочки, поэтому результат не
    /// зависит от порядка каналов в кадре.
    pub(super) fn for_decoder_channels(
        decoder_channels: &[ChannelPosition],
    ) -> Result<Self, UnconvertibleLayout> {
        let unconvertible = || UnconvertibleLayout {
            decoder_channels: decoder_channels.to_vec(),
        };
        let mut neutral_positions: Vec<Option<AudioChannelPosition>> = decoder_channels
            .iter()
            .map(|position| fixed_neutral_position(*position))
            .collect();
        for (decoder_index, decoder_position) in decoder_channels.iter().enumerate() {
            if neutral_positions
                .get(decoder_index)
                .is_some_and(Option::is_some)
            {
                continue;
            }
            let free_position = surround_direct_fallbacks(*decoder_position)
                .iter()
                .copied()
                .find(|candidate| !neutral_positions.contains(&Some(*candidate)))
                .ok_or_else(unconvertible)?;
            if let Some(slot) = neutral_positions.get_mut(decoder_index) {
                *slot = Some(free_position);
            }
        }
        let resolved: Vec<AudioChannelPosition> = neutral_positions
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(unconvertible)?;
        // `positioned` отвергает пустой список и повторы (два канала декодера на
        // одном месте) — такую раскладку тоже нельзя передать без потерь.
        let layout = AudioChannelLayout::positioned(&resolved).map_err(|_| unconvertible())?;
        let decoder_channel_for_lane = (0..resolved.len())
            .map(|lane| {
                let lane_position = layout.position_at(lane)?;
                resolved
                    .iter()
                    .position(|position| *position == lane_position)
            })
            .collect::<Option<Vec<usize>>>()
            .ok_or_else(unconvertible)?;
        Ok(Self {
            layout,
            decoder_channel_for_lane,
        })
    }

    /// Canonical layout кадра.
    pub(super) const fn layout(&self) -> AudioChannelLayout {
        self.layout
    }

    /// Индекс канала декодера для каждого canonical lane (для тестов перестановки).
    #[cfg(test)]
    pub(super) fn decoder_channel_for_lane(&self) -> &[usize] {
        &self.decoder_channel_for_lane
    }

    /// Перемежает планарный PCM декодера в canonical порядке, не меняя значений.
    ///
    /// Масштаб не трогается: 1,0 декодера — полная шкала neutral boundary, пики
    /// выше 0 dBFS сохраняются (ограничение — дело output-а).
    pub(super) fn interleave(&self, decoded: &DecodedAudio) -> Result<Vec<f32>, Ac3DecodeError> {
        if decoded.channels().len() != self.decoder_channel_for_lane.len() {
            return Err(Ac3DecodeError::InvalidChannelDescription {
                reason: format!(
                    "decoder produced {} channels, layout expects {}",
                    decoded.channels().len(),
                    self.decoder_channel_for_lane.len()
                ),
            });
        }
        let lanes = self
            .decoder_channel_for_lane
            .iter()
            .map(|channel| {
                decoded.channel_samples(*channel).ok_or_else(|| {
                    Ac3DecodeError::InvalidChannelDescription {
                        reason: format!("decoder channel {channel} has no samples"),
                    }
                })
            })
            .collect::<Result<Vec<&[f32]>, _>>()?;
        let frames = decoded.samples_per_channel();
        let mut interleaved = Vec::with_capacity(frames * lanes.len());
        for frame in 0..frames {
            // `channel_samples` возвращает ровно `samples_per_channel` значений,
            // поэтому индекс `frame` всегда в пределах каждого lane.
            interleaved.extend(lanes.iter().map(|lane| lane[frame]));
        }
        Ok(interleaved)
    }
}

/// Последняя рассчитанная перестановка вместе с раскладкой, для которой она верна.
#[derive(Debug)]
struct CachedConversion {
    /// Позиции каналов декодера, для которых посчитан `conversion`.
    decoder_channels: Vec<ChannelPosition>,
    /// Результат сопоставления (в том числе отказ — его тоже не пересчитываем).
    conversion: Result<NeutralLayout, UnconvertibleLayout>,
}

/// Владелец кэша перестановки: раскладка меняется редко (смена программы).
#[derive(Debug, Default)]
pub(super) struct LayoutConverter {
    /// `None` до первого кадра.
    cached: Option<CachedConversion>,
}

impl LayoutConverter {
    /// Возвращает перестановку для раскладки кадра, пересчитывая её только при смене.
    pub(super) fn conversion_for(
        &mut self,
        decoder_channels: &[ChannelPosition],
    ) -> &Result<NeutralLayout, UnconvertibleLayout> {
        let cached = self
            .cached
            .take_if(|cached| cached.decoder_channels.as_slice() == decoder_channels);
        let cached = cached.unwrap_or_else(|| CachedConversion {
            decoder_channels: decoder_channels.to_vec(),
            conversion: NeutralLayout::for_decoder_channels(decoder_channels),
        });
        &self.cached.insert(cached).conversion
    }
}
