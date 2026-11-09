//! Тексты меню качества. Английские: это интерфейс плеера.

use std::time::Duration;

use pushed_media_source::MediaBitrate;

use super::QualityMenuContent;

/// Что писать, когда значения нет.
const UNKNOWN_VALUE: &str = "—";

/// Верхний предел частоты кадров, которой ещё верим (выше — оценка длительности кадра
/// ещё не устоялась).
const MAX_BELIEVABLE_FRAMES_PER_SECOND: u32 = 480;

/// Строки меню качества.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityMenuText {
    /// «1920 × 1080».
    pub(crate) resolution: String,
    /// «850 kbps», «4.3 Mbps» или «—».
    pub(crate) bitrate: String,
    /// «H.264» или идентификатор кодека как есть, «—», если неизвестен.
    pub(crate) codec: String,
    /// «60 fps»; `None` — не показывать строку.
    pub(crate) frame_rate: Option<String>,
}

impl QualityMenuText {
    /// Тексты для содержимого меню.
    pub(super) fn of(content: &QualityMenuContent) -> Self {
        let picture = content.picture();
        Self {
            resolution: format!("{} × {}", picture.width, picture.height),
            bitrate: bitrate_text(content.bitrate().map(MediaBitrate::bits_per_second)),
            codec: codec_text(content.codec_id()),
            frame_rate: content
                .frames_per_second()
                .map(|frames| format!("{frames} fps")),
        }
    }
}

/// Битрейт: ниже 1 Мбит/с — целые килобиты, иначе мегабиты с одним знаком.
pub(super) fn bitrate_text(bits_per_second: Option<u64>) -> String {
    let Some(bits) = bits_per_second else {
        return UNKNOWN_VALUE.to_owned();
    };
    // Округление до ближайшего килобита; 999 700 бит/с — уже «1.0 Mbps», а не «1000 kbps».
    let kilobits = ((bits + 500) / 1_000).max(1);
    if kilobits < 1_000 {
        return format!("{kilobits} kbps");
    }
    let tenths_of_megabit = (bits + 50_000) / 100_000;
    format!("{}.{} Mbps", tenths_of_megabit / 10, tenths_of_megabit % 10)
}

/// Человеческое имя кодека; неизвестный идентификатор показываем как есть.
pub(super) fn codec_text(codec_id: Option<&str>) -> String {
    let Some(codec_id) = codec_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return UNKNOWN_VALUE.to_owned();
    };
    codec_core::VideoCodec::from_container_codec_id(codec_id).map_or_else(
        || codec_id.to_owned(),
        |codec| codec.display_name().to_owned(),
    )
}

/// Частота кадров по оценке длительности кадра; `None` — оценки нет или она неправдоподобна.
pub(super) fn frames_per_second(frame_duration: Duration) -> Option<u32> {
    let micros = frame_duration.as_micros();
    if micros == 0 {
        return None;
    }
    // Целочисленное округление: 16 683 мкс (59,94 кадра/с) → 60.
    let frames = u32::try_from((1_000_000 + micros / 2) / micros).ok()?;
    (1..=MAX_BELIEVABLE_FRAMES_PER_SECOND)
        .contains(&frames)
        .then_some(frames)
}
