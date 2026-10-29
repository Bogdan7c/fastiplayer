//! Текстовые сообщения сеанса от экстеншена (страницы) к плееру.
//!
//! Первое сообщение — всегда `attach` с пропуском. Дальше экстеншен пересказывает,
//! что плеер страницы делает с MSE (это нужно приёмнику, чтобы собрать байты
//! рамок в правильные дорожки), и сообщает события страницы.

use serde::{Deserialize, Serialize};

use crate::codec::ProtocolMessage;
use crate::error::ValueViolation;
use crate::session::variants::VariantMenu;
use crate::stream::{RangeRequestId, SeekId, SourceBufferIndex, StreamGeneration};
use crate::text::{MimeType, SessionKey, SessionToken};
use crate::time::{ByteOffset, MediaDuration, MediaTime, MediaTimeOffset, PlaybackRate};

/// Где кончается удаляемый диапазон `SourceBuffer.remove(start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemovalEnd {
    /// Конец в этой точке (не включая её).
    At {
        /// Конец диапазона.
        time_us: MediaTime,
    },
    /// До конца шкалы (`remove(start, Infinity)`).
    EndOfTimeline {},
}

/// Кто начал перемотку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SeekCause {
    /// Плеер попросил командой `seek` с этим номером.
    PlayerSeek {
        /// Номер из команды `seek`.
        seek_id: SeekId,
    },
    /// Плеер страницы перемотал сам (`video.currentTime = …`, адаптер, сайт).
    PageSeek {},
}

/// Чего плеер страницы хочет от «элемента» (муляж передаёт это плееру).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PagePlaybackRequest {
    /// Страница вызвала `play()` (например, автопереход к следующему ролику).
    Play {},
    /// Страница вызвала `pause()`.
    Pause {},
    /// Страница поменяла `playbackRate`.
    SetRate {
        /// Новая скорость.
        rate: PlaybackRate,
    },
}

/// Размер файла `<video src>`, если сервер его сообщил.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceSize {
    /// Размер известен (`Content-Range`).
    Known {
        /// Полный размер файла.
        size_bytes: ByteOffset,
    },
    /// Сервер размер не сообщил.
    Unknown {},
}

/// Почему запрос байтов `<video src>` не выполнен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeFetchFailure {
    /// Сеть недоступна или соединение оборвалось.
    NetworkError,
    /// Сервер ответил ошибкой HTTP.
    HttpError,
    /// Файл на сервере сменился между запросами (другой `ETag` или размер).
    ResourceChanged,
    /// Запрос за пределами файла.
    OutOfRange,
}

/// Чем кончился запрос байтов `<video src>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RangeFetchOutcome {
    /// Все байты запроса отправлены рамками (последний кусок файла может быть короче).
    Completed {
        /// Размер файла.
        resource_size: ResourceSize,
    },
    /// Запрос не выполнен.
    Failed {
        /// Почему.
        failure: RangeFetchFailure,
    },
}

/// Почему вкладка больше не источник (решение 42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabGoneReason {
    /// Вкладку закрыли.
    Closed,
    /// Во вкладке открыли другую страницу без видео того же сайта.
    NavigatedAway,
    /// Браузер закрывается.
    BrowserClosing,
}

/// К какому потоку относится сообщение — для сторожа поколений
/// (`crate::stream_gate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamScope {
    /// Не про поток (`attach`, перемотка элемента, события страницы).
    Unbound,
    /// Про поколение в целом (длительность, конец потока).
    Generation(StreamGeneration),
    /// Про один SourceBuffer поколения.
    SourceBuffer(StreamGeneration, SourceBufferIndex),
}

/// Текстовое сообщение сеанса от экстеншена к плееру.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PageSessionMessage {
    /// Первое сообщение соединения: предъявить пропуск.
    Attach {
        /// Ключ сеанса из `session_granted`.
        session_key: SessionKey,
        /// Одноразовый пропуск из `session_granted`.
        token: SessionToken,
    },
    /// Новый `MediaSource` на элементе: прежнее поколение закончилось.
    GenerationStarted {
        /// Номер нового поколения.
        generation: StreamGeneration,
    },
    /// `addSourceBuffer(mime)`.
    SourceBufferAdded {
        /// Поколение.
        generation: StreamGeneration,
        /// Индекс нового SourceBuffer.
        source_buffer: SourceBufferIndex,
        /// MIME с кодеками.
        mime_type: MimeType,
    },
    /// `changeType(mime)`: дальше в этом SourceBuffer другой кодек.
    SourceBufferTypeChanged {
        /// Поколение.
        generation: StreamGeneration,
        /// SourceBuffer.
        source_buffer: SourceBufferIndex,
        /// Новый MIME с кодеками.
        mime_type: MimeType,
    },
    /// `timestampOffset = …`.
    TimestampOffsetChanged {
        /// Поколение.
        generation: StreamGeneration,
        /// SourceBuffer.
        source_buffer: SourceBufferIndex,
        /// Новое смещение.
        offset_us: MediaTimeOffset,
    },
    /// `remove(start, end)`.
    SourceBufferRemoved {
        /// Поколение.
        generation: StreamGeneration,
        /// SourceBuffer.
        source_buffer: SourceBufferIndex,
        /// Начало диапазона.
        start_us: MediaTime,
        /// Конец диапазона.
        end: RemovalEnd,
    },
    /// `abort()`: незаконченный кусок брошен, разбор начинается заново.
    SourceBufferAborted {
        /// Поколение.
        generation: StreamGeneration,
        /// SourceBuffer.
        source_buffer: SourceBufferIndex,
    },
    /// Плеер страницы начал перемотку: дальше придут байты для новой точки.
    ///
    /// Номера потока не меняются и байты не выбрасываются (`crate::stream`):
    /// разрыв времени приёмник видит по меткам времени в байтах.
    SeekStarted {
        /// Куда перематывают.
        target_us: MediaTime,
        /// Кто начал.
        cause: SeekCause,
    },
    /// `MediaSource.duration` изменилась.
    DurationChanged {
        /// Поколение.
        generation: StreamGeneration,
        /// Новая длина.
        duration: MediaDuration,
    },
    /// `endOfStream()`: байтов этого поколения больше не будет.
    EndOfStream {
        /// Поколение.
        generation: StreamGeneration,
    },
    /// Плеер страницы сам вызвал `play`/`pause`/скорость.
    PagePlayback {
        /// Чего хочет страница.
        request: PagePlaybackRequest,
    },
    /// Адаптер прислал меню вариантов.
    VariantMenuChanged {
        /// Меню целиком.
        menu: VariantMenu,
    },
    /// Запрос байтов `<video src>` закончен.
    RangeFetchFinished {
        /// Номер запроса из `fetch_range`.
        request_id: RangeRequestId,
        /// Итог.
        outcome: RangeFetchOutcome,
    },
    /// Вкладка больше не источник: плеер доигрывает скачанное (решение 42).
    TabGone {
        /// Почему.
        reason: TabGoneReason,
    },
}

impl PageSessionMessage {
    /// К какому потоку относится сообщение.
    #[must_use]
    pub const fn stream_scope(&self) -> StreamScope {
        match self {
            Self::Attach { .. }
            | Self::PagePlayback { .. }
            | Self::VariantMenuChanged { .. }
            | Self::RangeFetchFinished { .. }
            | Self::TabGone { .. }
            | Self::SeekStarted { .. } => StreamScope::Unbound,
            Self::GenerationStarted { generation }
            | Self::DurationChanged { generation, .. }
            | Self::EndOfStream { generation } => StreamScope::Generation(*generation),
            Self::SourceBufferAdded {
                generation,
                source_buffer,
                ..
            }
            | Self::SourceBufferTypeChanged {
                generation,
                source_buffer,
                ..
            }
            | Self::TimestampOffsetChanged {
                generation,
                source_buffer,
                ..
            }
            | Self::SourceBufferRemoved {
                generation,
                source_buffer,
                ..
            }
            | Self::SourceBufferAborted {
                generation,
                source_buffer,
            } => StreamScope::SourceBuffer(*generation, *source_buffer),
        }
    }
}

impl ProtocolMessage for PageSessionMessage {
    const KNOWN_TYPES: &'static [&'static str] = &[
        "attach",
        "generation_started",
        "source_buffer_added",
        "source_buffer_type_changed",
        "timestamp_offset_changed",
        "source_buffer_removed",
        "source_buffer_aborted",
        "seek_started",
        "duration_changed",
        "end_of_stream",
        "page_playback",
        "variant_menu_changed",
        "range_fetch_finished",
        "tab_gone",
    ];

    fn validate(&self) -> Result<(), ValueViolation> {
        match self {
            Self::SourceBufferRemoved {
                start_us,
                end: RemovalEnd::At { time_us },
                ..
            } if time_us <= start_us => Err(ValueViolation::EmptyRange),
            Self::VariantMenuChanged { menu } => menu.validate(),
            _ => Ok(()),
        }
    }
}
