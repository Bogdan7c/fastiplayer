//! Перевод раскладки каналов FFmpeg в нейтральные позиции плеера.
//!
//! Модуль чистый (без FFI), поэтому его правила закреплены тестами в любой
//! сборке. FFI-слой передаёт сюда уже прочитанные числа: порядок раскладки,
//! число каналов и битовую маску. Значения `AVChannel` здесь — обычные числа,
//! а не Rust-`enum` из биндингов: значение вне списка вариантов в Rust-`enum`
//! было бы неопределённым поведением.
//!
//! Плеер хранит каналы в каноническом порядке `audio-core`. FFmpeg кладёт
//! каналы в своём порядке («native» — по возрастанию номера `AVChannel`).
//! Поэтому результат — раскладка плюс перестановка «канонический канал k
//! берётся из плоскости FFmpeg с номером source_plane_for_lane[k]».

use audio_core::{AudioChannelLayout, AudioChannelPosition};

use crate::error::AudioFfmpegError;

/// Порядок каналов кадра FFmpeg (`AVChannelOrder`), прочитанный как число.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfmpegChannelOrder {
    /// `AV_CHANNEL_ORDER_UNSPEC`: известно только число каналов.
    Unspecified,
    /// `AV_CHANNEL_ORDER_NATIVE`: каналы перечислены битами маски по возрастанию.
    Native {
        /// Битовая маска: бит N установлен — канал `AVChannel` с номером N есть.
        mask: u64,
    },
    /// Любой другой порядок (CUSTOM, AMBISONIC, неизвестный): не поддерживаем.
    Other {
        /// Числовое значение `AVChannelOrder` для диагностики.
        raw_order: i32,
    },
}

/// Нейтральная раскладка кадра и перестановка плоскостей FFmpeg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelMapping {
    /// Раскладка в каноническом порядке плеера.
    pub(crate) layout: AudioChannelLayout,
    /// Для канонического канала k — номер плоскости FFmpeg, из которой он берётся.
    pub(crate) source_plane_for_lane: Vec<usize>,
}

/// Переводит раскладку кадра FFmpeg в нейтральную.
pub(crate) fn map_ffmpeg_channels(
    order: FfmpegChannelOrder,
    channel_count: usize,
) -> Result<ChannelMapping, AudioFfmpegError> {
    match order {
        FfmpegChannelOrder::Native { mask } => map_native_mask(mask, channel_count),
        FfmpegChannelOrder::Unspecified => map_unspecified(channel_count),
        FfmpegChannelOrder::Other { raw_order } => Err(unsupported(format!(
            "channel order {raw_order} with {channel_count} channels"
        ))),
    }
}

/// Native-порядок: плоскость i — i-й установленный бит маски по возрастанию.
fn map_native_mask(mask: u64, channel_count: usize) -> Result<ChannelMapping, AudioFfmpegError> {
    let ffmpeg_channel_ids: Vec<u32> = (0..u64::BITS)
        .filter(|bit| mask & (1 << bit) != 0)
        .collect();
    if ffmpeg_channel_ids.len() != channel_count || channel_count == 0 {
        return Err(unsupported(format!(
            "native mask {mask:#x} has {} channels, frame says {channel_count}",
            ffmpeg_channel_ids.len()
        )));
    }

    // Позиция каждой плоскости FFmpeg в её собственном порядке.
    let plane_positions = ffmpeg_channel_ids
        .iter()
        .map(|channel_id| {
            position_for_ffmpeg_channel(*channel_id).ok_or_else(|| {
                unsupported(format!(
                    "native mask {mask:#x} contains FFmpeg channel {channel_id} without a player position"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let layout = AudioChannelLayout::positioned(&plane_positions)
        .map_err(|error| unsupported(format!("native mask {mask:#x}: {error}")))?;
    let source_plane_for_lane = canonical_lane_sources(layout, &plane_positions)?;
    Ok(ChannelMapping {
        layout,
        source_plane_for_lane,
    })
}

/// Без позиций честно понимаем только моно и стерео; остальное — отказ,
/// чтобы не гадать, где центр, а где surround.
fn map_unspecified(channel_count: usize) -> Result<ChannelMapping, AudioFfmpegError> {
    if !(1..=2).contains(&channel_count) {
        return Err(unsupported(format!(
            "unspecified order with {channel_count} channels"
        )));
    }
    let channel_count_u32 = u32::try_from(channel_count)
        .map_err(|_| unsupported(format!("unspecified order with {channel_count} channels")))?;
    let layout = AudioChannelLayout::from_channel_count(channel_count_u32)
        .map_err(|error| unsupported(format!("unspecified order: {error}")))?;
    Ok(ChannelMapping {
        layout,
        // Моно и стерео без позиций: порядок FFmpeg совпадает с каноническим.
        source_plane_for_lane: (0..channel_count).collect(),
    })
}

/// Для каждого канонического канала ищет плоскость FFmpeg с той же позицией.
fn canonical_lane_sources(
    layout: AudioChannelLayout,
    plane_positions: &[AudioChannelPosition],
) -> Result<Vec<usize>, AudioFfmpegError> {
    (0..plane_positions.len())
        .map(|lane_index| {
            let lane_position = layout
                .position_at(lane_index)
                .ok_or_else(|| unsupported(format!("canonical layout has no lane {lane_index}")))?;
            plane_positions
                .iter()
                .position(|plane_position| *plane_position == lane_position)
                .ok_or_else(|| unsupported(format!("no FFmpeg plane for {lane_position:?}")))
        })
        .collect()
}

/// Номер `AVChannel` (FFmpeg 8.1, `channel_layout.h`) → позиция плеера.
///
/// «Back» у FFmpeg — это «Rear» у плеера. Каналы без нейтральной позиции
/// (стерео-даунмикс, surround direct, side surround, бинауральные) — `None`.
fn position_for_ffmpeg_channel(channel_id: u32) -> Option<AudioChannelPosition> {
    use AudioChannelPosition as Position;
    Some(match channel_id {
        0 => Position::FrontLeft,
        1 => Position::FrontRight,
        2 => Position::FrontCenter,
        3 => Position::LowFrequencyEffects,
        4 => Position::RearLeft,
        5 => Position::RearRight,
        6 => Position::FrontLeftOfCenter,
        7 => Position::FrontRightOfCenter,
        8 => Position::RearCenter,
        9 => Position::SideLeft,
        10 => Position::SideRight,
        11 => Position::TopCenter,
        12 => Position::TopFrontLeft,
        13 => Position::TopFrontCenter,
        14 => Position::TopFrontRight,
        15 => Position::TopRearLeft,
        16 => Position::TopRearCenter,
        17 => Position::TopRearRight,
        31 => Position::FrontLeftWide,
        32 => Position::FrontRightWide,
        35 => Position::LowFrequencyEffects2,
        36 => Position::TopSideLeft,
        37 => Position::TopSideRight,
        38 => Position::BottomFrontCenter,
        39 => Position::BottomFrontLeft,
        40 => Position::BottomFrontRight,
        _ => return None,
    })
}

fn unsupported(details: String) -> AudioFfmpegError {
    AudioFfmpegError::UnsupportedChannelLayout { details }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Маска FFmpeg из номеров каналов.
    fn mask_of(channel_ids: &[u32]) -> u64 {
        channel_ids.iter().fold(0, |mask, id| mask | (1 << id))
    }

    #[test]
    fn stereo_native_mask_keeps_left_and_right_planes() {
        let mapping = map_ffmpeg_channels(
            FfmpegChannelOrder::Native {
                mask: mask_of(&[0, 1]),
            },
            2,
        )
        .expect("stereo maps");
        assert_eq!(
            mapping.layout.position_at(0),
            Some(AudioChannelPosition::FrontLeft)
        );
        assert_eq!(
            mapping.layout.position_at(1),
            Some(AudioChannelPosition::FrontRight)
        );
        assert_eq!(mapping.source_plane_for_lane, vec![0, 1]);
    }

    #[test]
    fn mono_native_mask_is_front_center() {
        let mapping = map_ffmpeg_channels(
            FfmpegChannelOrder::Native {
                mask: mask_of(&[2]),
            },
            1,
        )
        .expect("mono maps");
        assert_eq!(mapping.layout.channel_count(), 1);
        assert_eq!(
            mapping.layout.position_at(0),
            Some(AudioChannelPosition::FrontCenter)
        );
    }

    #[test]
    fn every_canonical_lane_takes_the_plane_with_the_same_position() {
        // 5.1(side) + широкие пары: позиции из разных частей таблицы FFmpeg.
        let ffmpeg_ids = [0, 1, 2, 3, 9, 10, 31, 32];
        let mapping = map_ffmpeg_channels(
            FfmpegChannelOrder::Native {
                mask: mask_of(&ffmpeg_ids),
            },
            ffmpeg_ids.len(),
        )
        .expect("layout maps");
        for (lane_index, source_plane) in mapping.source_plane_for_lane.iter().enumerate() {
            let lane_position = mapping.layout.position_at(lane_index);
            let plane_position = position_for_ffmpeg_channel(ffmpeg_ids[*source_plane]);
            assert_eq!(lane_position, plane_position, "lane {lane_index}");
        }
    }

    #[test]
    fn mask_and_frame_channel_count_must_agree() {
        let result = map_ffmpeg_channels(
            FfmpegChannelOrder::Native {
                mask: mask_of(&[0, 1]),
            },
            3,
        );
        assert!(matches!(
            result,
            Err(AudioFfmpegError::UnsupportedChannelLayout { .. })
        ));
    }

    #[test]
    fn channels_without_player_position_are_refused() {
        // 29/30 — стерео-даунмикс FFmpeg, у плеера такой позиции нет.
        let result = map_ffmpeg_channels(
            FfmpegChannelOrder::Native {
                mask: mask_of(&[29, 30]),
            },
            2,
        );
        assert!(matches!(
            result,
            Err(AudioFfmpegError::UnsupportedChannelLayout { .. })
        ));
    }

    #[test]
    fn unspecified_order_is_only_trusted_for_mono_and_stereo() {
        assert_eq!(
            map_ffmpeg_channels(FfmpegChannelOrder::Unspecified, 2)
                .expect("stereo")
                .layout
                .channel_count(),
            2
        );
        assert!(map_ffmpeg_channels(FfmpegChannelOrder::Unspecified, 6).is_err());
        assert!(map_ffmpeg_channels(FfmpegChannelOrder::Unspecified, 0).is_err());
    }

    #[test]
    fn custom_and_ambisonic_orders_are_refused() {
        for raw_order in [2, 3, 99] {
            let result = map_ffmpeg_channels(FfmpegChannelOrder::Other { raw_order }, 2);
            assert!(matches!(
                result,
                Err(AudioFfmpegError::UnsupportedChannelLayout { .. })
            ));
        }
    }
}
