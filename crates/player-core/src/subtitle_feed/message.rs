//! Сообщения трубы субтитров.

use std::time::Duration;

use bytes::Bytes;
use media_core::{MediaAttachments, TrackId};

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
