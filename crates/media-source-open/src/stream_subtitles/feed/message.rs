//! Сообщения ленты субтитров приложению.

use std::fmt;

use subtitle_core::SubtitleEvent;

use crate::stream_subtitles::error::StreamSubtitleFailureKind;
use crate::stream_subtitles::transport::{PlayerTimeSpan, StreamSegmentKey};

/// Почему сегмент не дал реплик.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSegmentSkipReason {
    /// Загрузка или разбор не удались (после встроенных повторов HTTP); считается в серию сбоев.
    Failed(StreamSubtitleFailureKind),
    /// Сегмент слишком долго ждал точку отсчёта времени и был вытеснен новыми.
    PendingOverflow,
    /// Живой плейлист продвинулся, сегмент вышел из окна раньше, чем его удалось привязать.
    LeftPlaylist,
}

/// Почему дорожка признана недоступной (сообщается один раз, затем лента останавливается).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTrackUnavailableReason {
    /// Плейлист/индекс дорожки не загрузился (для live - несколько раз подряд).
    IndexLoadFailed(StreamSubtitleFailureKind),
    /// Подряд не удались столько сегментов, сколько задаёт порог.
    ConsecutiveSegmentFailures {
        /// Сколько сегментов подряд не удалось.
        count: u32,
    },
    /// Поток ленты погиб аварийно (паника) и больше ничего не пришлёт: канал закрылся без
    /// терминального сообщения и без отмены. Сообщение синтезирует владелец ленты.
    WorkerLost,
}

/// Пачка реплик одного сегмента на шкале плеера.
#[derive(Clone, PartialEq)]
pub struct StreamSubtitleEvents {
    /// Из какого сегмента реплики.
    pub segment: StreamSegmentKey,
    /// Реплики (порядок не гарантируется; может быть пусто).
    pub events: Vec<SubtitleEvent>,
    /// Участок шкалы плеера, который сегмент покрывает.
    pub covered: Option<PlayerTimeSpan>,
}

impl fmt::Debug for StreamSubtitleEvents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamSubtitleEvents")
            .field("segment", &self.segment)
            .field("events", &self.events.len())
            .field("covered", &self.covered)
            .finish()
    }
}

/// Сообщение ленты. Адресов и секретов не содержит.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamSubtitleMessage {
    /// Новые реплики.
    Events(StreamSubtitleEvents),
    /// Сегмент пропущен; видео и остальные сегменты не страдают.
    SegmentSkipped {
        /// Ключ пропущенного сегмента.
        segment: StreamSegmentKey,
        /// Причина.
        reason: StreamSegmentSkipReason,
    },
    /// Дорожка недоступна. Приходит ровно один раз, после чего лента остановлена;
    /// повтор - только новой лентой.
    TrackUnavailable(StreamTrackUnavailableReason),
    /// Загрузка снова пошла после серии пропущенных сегментов (порог ещё не был достигнут).
    Recovered,
}
