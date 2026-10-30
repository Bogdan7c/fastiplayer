//! Текстовые сообщения сеанса от плеера к экстеншену (странице).
//!
//! Часы ведёт плеер (`results/00c.md`, п. 4): экстеншен получает позицию,
//! паузу, скорость, «жду данных» и «конец» и сам генерирует события
//! муляжа видеоэлемента. Тики идут по этому соединению, а не таймерами
//! страницы: в фоне Chromium будит их раз в минуту.

use serde::{Deserialize, Serialize};

use crate::codec::ProtocolMessage;
use crate::error::ValueViolation;
use crate::session::variants::VariantKind;
use crate::stream::{RangeRequestId, SeekId, StreamGeneration};
use crate::text::VariantId;
use crate::time::{ByteOffset, MediaTime, PlaybackRate, RangeFetchLength};

/// Номер тика часов: страница по нему отбрасывает запоздавшие тики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClockSequence(pub u32);

/// Что сейчас делает плеер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockState {
    /// Играет.
    Playing,
    /// На паузе.
    Paused,
    /// Хочет играть, но ждёт данных (муляж отдаёт `waiting`).
    Waiting,
    /// Дошёл до конца (муляж отдаёт `ended`, `results/00c.md`, п. 7).
    Ended,
}

/// Принимает ли плеер байты дальше (обратное давление).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferIntake {
    /// Места хватает.
    Accepting,
    /// Буфер полон: экстеншен придерживает рамки, а плеер страницы и сам
    /// перестаёт качать, когда его цель буфера достигнута.
    Full,
}

/// В каком состоянии вернуть видео на страницу (решение 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeState {
    /// Страница играет дальше (кнопка «Return»).
    Playing,
    /// Страница на паузе (закрыли плеер, заменили из другой вкладки).
    Paused,
}

/// Почему плеер не будет играть это поколение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamRejection {
    /// В init-сегменте шифрование (`encv`/`enca`/`tenc`): последняя линия
    /// против DRM, если признак на странице не пришёл (Vimeo, `results/00c.md`).
    ProtectedContent,
    /// Кодек плееру не по силам.
    UnsupportedCodec,
    /// Байты не разбираются как fMP4/WebM.
    UnparsableMedia,
    /// Файл `<video src>` получить не удалось окончательно: страница сообщила,
    /// что файл на сервере поменялся, или исчерпаны повторы после сетевых ошибок.
    FileUnavailable,
}

/// Текстовое сообщение сеанса от плеера к экстеншену.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlayerSessionMessage {
    /// Пропуск принят, сеанс начался.
    Attached {},
    /// Тик часов плеера.
    Clock {
        /// Номер тика.
        seq: ClockSequence,
        /// Позиция плеера.
        position_us: MediaTime,
        /// Что делает плеер.
        state: ClockState,
        /// Скорость.
        rate: PlaybackRate,
    },
    /// Сколько буфера впереди и принимает ли плеер ещё.
    BufferState {
        /// Поколение, о котором речь.
        generation: StreamGeneration,
        /// Сколько впереди позиции уже разобрано.
        ahead_us: MediaTime,
        /// Обратное давление.
        intake: BufferIntake,
    },
    /// Играть (страница должна качать дальше).
    Play {},
    /// Пауза.
    Pause {},
    /// Перемотать плеер страницы.
    Seek {
        /// Номер, который вернётся в `seek_started`.
        seek_id: SeekId,
        /// Куда.
        target_us: MediaTime,
    },
    /// Скорость (плеер страницы на 2× качает быстрее, `results/00c.md`).
    SetRate {
        /// Новая скорость.
        rate: PlaybackRate,
    },
    /// Вернуть видео на страницу: с этой секунды и в этом состоянии.
    ReturnToPage {
        /// С какой секунды.
        position_us: MediaTime,
        /// Играть или пауза.
        resume: ResumeState,
    },
    /// Выбрать вариант в меню страницы (озвучка, качество, субтитры).
    SelectVariant {
        /// Какое меню.
        kind: VariantKind,
        /// Какой вариант.
        variant_id: VariantId,
    },
    /// Следующая серия через адаптер.
    NextEpisode {},
    /// Дай байты файла `<video src>` (качает background экстеншена).
    FetchRange {
        /// Номер запроса.
        request_id: RangeRequestId,
        /// С какого байта.
        offset: ByteOffset,
        /// Сколько байт.
        length: RangeFetchLength,
    },
    /// Плеер не будет играть это поколение — экстеншен возвращает видео странице.
    StreamRejected {
        /// Поколение.
        generation: StreamGeneration,
        /// Почему.
        reason: StreamRejection,
    },
}

impl ProtocolMessage for PlayerSessionMessage {
    const KNOWN_TYPES: &'static [&'static str] = &[
        "attached",
        "clock",
        "buffer_state",
        "play",
        "pause",
        "seek",
        "set_rate",
        "return_to_page",
        "select_variant",
        "next_episode",
        "fetch_range",
        "stream_rejected",
    ];

    fn validate(&self) -> Result<(), ValueViolation> {
        // Все правила этих сообщений выражены типами полей.
        Ok(())
    }
}
