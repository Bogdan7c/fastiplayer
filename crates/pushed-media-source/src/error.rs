//! Ошибки потока и их грубые классы для того, кто приносит байты.
//!
//! Любая ошибка — это ошибка одного потока: процесс живёт, плеер получает
//! ошибку чтения демуксера, а владелец канала (переходник протокола 03b) по
//! классу ошибки сообщает странице причину отказа.

use crate::framing::FramingError;
use crate::lane::LaneId;

/// Что не так с раскладкой полос для проигрывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneLayoutProblem {
    /// Ни одна полоса не принесла медиа до конца входа.
    NoMedia,
    /// Полос с медиа больше, чем умеет соединить плеер (видео + звук = 2).
    TooManyMediaLanes {
        /// Сколько полос принесли медиа.
        count: usize,
    },
    /// Две полосы, но не «одна с видео, другая со звуком».
    NotOneVideoAndOneAudio,
}

/// Ошибка потока.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedStreamError {
    /// Полоса не объявлена.
    #[error("lane {lane} was not declared")]
    UnknownLane {
        /// Полоса.
        lane: LaneId,
    },
    /// Полоса уже объявлена.
    #[error("lane {lane} is already declared")]
    LaneAlreadyDeclared {
        /// Полоса.
        lane: LaneId,
    },
    /// Слишком много полос.
    #[error("a stream may have at most {limit} lanes")]
    TooManyLanes {
        /// Потолок.
        limit: usize,
    },
    /// Полосу добавили после того, как поток уже отдан плееру.
    #[error("lane {lane} was declared after playback had opened")]
    LayoutFrozen {
        /// Полоса.
        lane: LaneId,
    },
    /// Раскладку полос плеер проиграть не может.
    #[error("unsupported lane layout: {problem:?}")]
    UnsupportedLayout {
        /// Что не так.
        problem: LaneLayoutProblem,
    },
    /// Байты полосы не складываются в сегменты контейнера.
    #[error("lane {lane}: {source}")]
    Framing {
        /// Полоса.
        lane: LaneId,
        /// Причина.
        source: FramingError,
    },
    /// Init-сегмент описывает зашифрованное видео.
    #[error("lane {lane} carries protected (encrypted) content")]
    ProtectedContent {
        /// Полоса.
        lane: LaneId,
    },
    /// Структура init-сегмента не сходится.
    #[error("lane {lane} has an inconsistent initialization segment")]
    InconsistentInitialization {
        /// Полоса.
        lane: LaneId,
    },
    /// Медиасегмент пришёл раньше init-сегмента.
    #[error("lane {lane} received media before an initialization segment")]
    MediaBeforeInitialization {
        /// Полоса.
        lane: LaneId,
    },
    /// Смещение времени для контейнера не поддерживается (MPEG-TS).
    #[error("lane {lane} does not support timestamp offsets for its container")]
    TimestampOffsetUnsupported {
        /// Полоса.
        lane: LaneId,
    },
    /// Поток держит больше байтов, чем позволяет потолок, и выбросить уже
    /// показанное нечего: страница шлёт, не слушая «полон».
    #[error("the stream would hold more than {limit} bytes")]
    IntakeLimitExceeded {
        /// Потолок.
        limit: usize,
    },
    /// Плеер не успевает забирать эпизоды: страница меняет init или смещение слишком часто.
    #[error("lane {lane} has more than {limit} episodes waiting for the player")]
    TooManyPendingEpisodes {
        /// Полоса.
        lane: LaneId,
        /// Потолок.
        limit: usize,
    },
    /// Демуксер потока уже выдан плееру; повторное открытие не поддерживается.
    #[error("the pushed stream was already opened")]
    AlreadyOpened,
    /// Не удалось запустить демуксер эпизода.
    #[error("lane {lane}: the episode demuxer could not start: {reason}")]
    EpisodeStart {
        /// Полоса.
        lane: LaneId,
        /// Причина.
        reason: String,
    },
    /// Демуксер не смог разобрать байты.
    #[error("lane {lane}: demux failed: {reason}")]
    Demux {
        /// Полоса.
        lane: LaneId,
        /// Причина.
        reason: String,
    },
}

/// Грубый класс ошибки — то, что можно честно сказать источнику байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushedStreamFailureClass {
    /// Защищённое видео: не играем принципиально.
    ProtectedContent,
    /// Формат или раскладка, которые плеер не поддерживает.
    UnsupportedMedia,
    /// Байты не разбираются.
    UnparsableMedia,
    /// Нарушен порядок вызовов (ошибка того, кто приносит байты).
    InputMisuse,
}

impl PushedStreamError {
    /// Класс ошибки для ответа источнику байтов.
    #[must_use]
    pub const fn failure_class(&self) -> PushedStreamFailureClass {
        match self {
            Self::ProtectedContent { .. } => PushedStreamFailureClass::ProtectedContent,
            Self::UnsupportedLayout { .. } | Self::TimestampOffsetUnsupported { .. } => {
                PushedStreamFailureClass::UnsupportedMedia
            }
            Self::Framing { .. }
            | Self::InconsistentInitialization { .. }
            | Self::EpisodeStart { .. }
            | Self::Demux { .. } => PushedStreamFailureClass::UnparsableMedia,
            Self::UnknownLane { .. }
            | Self::LaneAlreadyDeclared { .. }
            | Self::TooManyLanes { .. }
            | Self::LayoutFrozen { .. }
            | Self::MediaBeforeInitialization { .. }
            | Self::IntakeLimitExceeded { .. }
            | Self::TooManyPendingEpisodes { .. }
            | Self::AlreadyOpened => PushedStreamFailureClass::InputMisuse,
        }
    }
}
