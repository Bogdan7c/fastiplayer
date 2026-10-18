//! Сообщения трубы субтитров.

use std::time::Duration;

use bytes::Bytes;
use media_core::{MediaAttachments, TrackId};

use super::{SubtitleReadRun, SubtitleTrackHeader};
use crate::MediaInstanceId;

/// Начало нового media: вложения (шрифты), забранные у demuxer при установке media.
///
/// Приходит ровно один раз на установку, раньше любых пакетов этого media.
/// Потеря этого сообщения (переполнение) означает потерю вложенных шрифтов, поэтому
/// она считается отдельным счётчиком (`dropped_media_starts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleFeedMediaStart {
    /// Установка media, к которой относятся вложения.
    pub media_instance_id: MediaInstanceId,

    /// Вложения контейнера (принятые и пропущенные по лимиту).
    pub attachments: MediaAttachments,

    /// Заголовки субтитровых дорожек (`codec_private`), например ASS `[Script Info]`.
    ///
    /// Только дорожки с непустым заголовком. Не входят в снимок плеера (см. `track_headers`).
    pub track_headers: Vec<SubtitleTrackHeader>,
}

/// Один пакет встроенной субтитровой дорожки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleFeedPacket {
    /// Установка media, из которой прочитан пакет (приложение отбрасывает чужие).
    pub media_instance_id: MediaInstanceId,

    /// Дорожка-источник.
    pub track_id: TrackId,

    /// `Packet::pts` демуксера: та же шкала, что у pts видеокадров (см. документацию модуля).
    pub presentation_time: Duration,

    /// Длительность реплики, если контейнер её сообщил.
    pub duration: Option<Duration>,

    /// Проход чтения демуксера, в котором пакет получен (растёт при каждом seek/scrub).
    pub read_run: SubtitleReadRun,

    /// Сырой payload пакета (формат зависит от кодека дорожки).
    pub payload: Bytes,
}

/// Сообщение трубы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtitleFeedMessage {
    /// Началось новое media (вложения).
    MediaStarted(SubtitleFeedMediaStart),

    /// Пакет встроенной дорожки.
    Packet(SubtitleFeedPacket),
}
