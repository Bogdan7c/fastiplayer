//! Лимиты субтитров потоковых источников `[subtitles.stream]` (схема v23): HLS-дорожки
//! WebVTT (в будущем DASH). Вынесены вложенной секцией, чтобы файл схемы `[subtitles]`
//! не раздувался; публичный путь констант и типа - `pub use` в `schema.rs`.
//!
//! Потоковые субтитры качаются кусками вокруг позиции воспроизведения (окно «вперёд» и
//! «назад»), поэтому лимиты задают окна времени и размеры, а не число реплик. Видео
//! субтитры не ждёт: любой сбой только пропускает сегмент.

use serde::{Deserialize, Serialize};

/// Окно «вперёд» от позиции, секунд, по умолчанию: минута реплик хватает, чтобы субтитры
/// не отставали при обычной скорости сети и перемотке на пару десятков секунд.
pub const DEFAULT_STREAM_AHEAD_S: u16 = 60;
/// Минимум окна «вперёд»: 5 с (сегмент HLS обычно 2-10 с).
pub const MIN_STREAM_AHEAD_S: u16 = 5;
/// Максимум окна «вперёд»: 10 минут.
pub const MAX_STREAM_AHEAD_S: u16 = 600;

/// Окно «назад» от позиции, секунд, по умолчанию: короткая перемотка назад не уходит в сеть.
pub const DEFAULT_STREAM_BEHIND_S: u16 = 30;
/// Минимум окна «назад»: 0 (прошлое не докачивается).
pub const MIN_STREAM_BEHIND_S: u16 = 0;
/// Максимум окна «назад»: 10 минут.
pub const MAX_STREAM_BEHIND_S: u16 = 600;

/// Максимальный размер одного сегмента субтитров, КБ, по умолчанию. Текстовый сегмент
/// WebVTT/TTML - единицы КБ; 1 МиБ с большим запасом.
pub const DEFAULT_STREAM_MAX_SEGMENT_KB: u16 = 1_024;
/// Минимум: 64 КБ.
pub const MIN_STREAM_MAX_SEGMENT_KB: u16 = 64;
/// Максимум: 16 МиБ (одиночный файл TTML целой серии).
pub const MAX_STREAM_MAX_SEGMENT_KB: u16 = 16_384;

/// Сколько сегментов может ждать точку отсчёта времени видео, по умолчанию.
pub const DEFAULT_STREAM_MAX_PENDING_SEGMENTS: u16 = 8;
/// Минимум: 1 сегмент.
pub const MIN_STREAM_MAX_PENDING_SEGMENTS: u16 = 1;
/// Максимум: 64 сегмента.
pub const MAX_STREAM_MAX_PENDING_SEGMENTS: u16 = 64;

/// После скольких подряд неудавшихся сегментов дорожка считается недоступной, по умолчанию.
pub const DEFAULT_STREAM_FAILURE_THRESHOLD: u16 = 3;
/// Минимум: 1 (первый же сбой).
pub const MIN_STREAM_FAILURE_THRESHOLD: u16 = 1;
/// Максимум: 20 сбоев подряд.
pub const MAX_STREAM_FAILURE_THRESHOLD: u16 = 20;

/// Сколько секунд реплик позади позиции хранит живая трансляция, по умолчанию: 10 минут.
/// Старее реплики вытесняются, иначе память на бесконечном потоке росла бы без предела.
pub const DEFAULT_STREAM_LIVE_RETENTION_S: u16 = 600;
/// Минимум: 30 с.
pub const MIN_STREAM_LIVE_RETENTION_S: u16 = 30;
/// Максимум: 2 часа.
pub const MAX_STREAM_LIVE_RETENTION_S: u16 = 7_200;

/// Лимиты субтитров потоковых источников.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct SubtitleStreamConfig {
    /// Сколько секунд субтитров качать вперёд от позиции воспроизведения.
    #[setting(
        id = "subtitles.stream.ahead_s",
        path = "subtitles.stream.ahead_s",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.ahead_s.label",
        label_ru = "Stream subtitles: look ahead",
        description_id = "settings.subtitles.stream.ahead_s.description",
        description_ru = "How many seconds of subtitles ahead of the playback position are downloaded for streams (HLS). Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_AHEAD_S,
        max = crate::MAX_STREAM_AHEAD_S,
        step = 5,
        unit = "s",
        apply = "subtitles.apply"
    )]
    pub ahead_s: u16,

    /// Сколько секунд субтитров докачивать назад от позиции воспроизведения.
    #[setting(
        id = "subtitles.stream.behind_s",
        path = "subtitles.stream.behind_s",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.behind_s.label",
        label_ru = "Stream subtitles: look behind",
        description_id = "settings.subtitles.stream.behind_s.description",
        description_ru = "How many seconds of subtitles behind the playback position are downloaded for streams, so a short rewind shows them at once. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_BEHIND_S,
        max = crate::MAX_STREAM_BEHIND_S,
        step = 5,
        unit = "s",
        apply = "subtitles.apply"
    )]
    pub behind_s: u16,

    /// Максимальный размер одного сегмента субтитров потока, КБ.
    #[setting(
        id = "subtitles.stream.max_segment_kb",
        path = "subtitles.stream.max_segment_kb",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.max_segment_kb.label",
        label_ru = "Stream subtitles: segment size limit",
        description_id = "settings.subtitles.stream.max_segment_kb.description",
        description_ru = "Subtitle segments of a stream larger than this are skipped. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_MAX_SEGMENT_KB,
        max = crate::MAX_STREAM_MAX_SEGMENT_KB,
        step = 64,
        unit = "KB",
        apply = "subtitles.apply"
    )]
    pub max_segment_kb: u16,

    /// Сколько сегментов может ждать точку отсчёта времени видео.
    #[setting(
        id = "subtitles.stream.max_pending_segments",
        path = "subtitles.stream.max_pending_segments",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.max_pending_segments.label",
        label_ru = "Stream subtitles: waiting segments",
        description_id = "settings.subtitles.stream.max_pending_segments.description",
        description_ru = "How many downloaded subtitle segments may wait until the video timeline origin is known; older ones are dropped when exceeded. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_MAX_PENDING_SEGMENTS,
        max = crate::MAX_STREAM_MAX_PENDING_SEGMENTS,
        step = 1,
        unit = "segments",
        apply = "subtitles.apply"
    )]
    pub max_pending_segments: u16,

    /// После скольких подряд неудавшихся сегментов дорожка считается недоступной.
    #[setting(
        id = "subtitles.stream.failure_threshold",
        path = "subtitles.stream.failure_threshold",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.failure_threshold.label",
        label_ru = "Stream subtitles: failures before giving up",
        description_id = "settings.subtitles.stream.failure_threshold.description",
        description_ru = "After this many subtitle segments in a row fail, the track is reported as unavailable (one notification); selecting it again retries. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_FAILURE_THRESHOLD,
        max = crate::MAX_STREAM_FAILURE_THRESHOLD,
        step = 1,
        unit = "segments",
        apply = "subtitles.apply"
    )]
    pub failure_threshold: u16,

    /// Сколько секунд реплик позади позиции хранит живая трансляция.
    #[setting(
        id = "subtitles.stream.live_retention_s",
        path = "subtitles.stream.live_retention_s",
        section = "subtitles",
        group = "stream",
        surface = "main-settings-window",
        label_id = "settings.subtitles.stream.live_retention_s.label",
        label_ru = "Live subtitles: history kept",
        description_id = "settings.subtitles.stream.live_retention_s.description",
        description_ru = "How many seconds of subtitles behind the playback position a live stream keeps; older cues are dropped so memory stays bounded. Must not be smaller than the look-behind window. Takes effect with the next opened video.",
        editor = "integer",
        min = crate::MIN_STREAM_LIVE_RETENTION_S,
        max = crate::MAX_STREAM_LIVE_RETENTION_S,
        step = 30,
        unit = "s",
        apply = "subtitles.apply"
    )]
    pub live_retention_s: u16,
}

impl Default for SubtitleStreamConfig {
    /// Значения по умолчанию - константы выше.
    fn default() -> Self {
        Self {
            ahead_s: DEFAULT_STREAM_AHEAD_S,
            behind_s: DEFAULT_STREAM_BEHIND_S,
            max_segment_kb: DEFAULT_STREAM_MAX_SEGMENT_KB,
            max_pending_segments: DEFAULT_STREAM_MAX_PENDING_SEGMENTS,
            failure_threshold: DEFAULT_STREAM_FAILURE_THRESHOLD,
            live_retention_s: DEFAULT_STREAM_LIVE_RETENTION_S,
        }
    }
}
