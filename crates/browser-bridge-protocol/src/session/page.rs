//! Текстовые сообщения сеанса от экстеншена (страницы) к плееру.
//!
//! Первое сообщение — всегда `attach` с пропуском. Дальше экстеншен пересказывает,
//! что плеер страницы делает с MSE (это нужно приёмнику, чтобы собрать байты
//! рамок в правильные дорожки), и сообщает события страницы.

use serde::{Deserialize, Serialize};

use crate::codec::ProtocolMessage;
use crate::error::ValueViolation;
use crate::resource::{ResourceFetchOutcome, ResourceHandle};
use crate::session::ad_state::PageAdState;
use crate::session::page_reopen::PageReopenFailure;
use crate::session::variants::VariantMenu;
use crate::stream::{RangeRequestId, SeekId, SourceBufferIndex, StreamGeneration};
use crate::strict_shape::{strict_label, strict_object};
use crate::text::{MimeType, SessionKey, SessionToken};
use crate::time::{ByteOffset, MediaDuration, MediaTime, MediaTimeOffset, PlaybackRate};
use crate::version::ProtocolVersion;

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(
    RemovalEnd,
    SeekCause,
    PagePlaybackRequest,
    ResourceSize,
    RangeFetchOutcome,
    PageSessionMessage
);
strict_label!(RangeFetchFailure, TabGoneReason);

/// Где кончается удаляемый диапазон `SourceBuffer.remove(start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
#[serde(remote = "Self", rename_all = "snake_case")]
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
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum TabGoneReason {
    /// Вкладку закрыли.
    Closed,
    /// Во вкладке открыли другую страницу без видео того же сайта.
    NavigatedAway,
    /// Браузер закрывается.
    BrowserClosing,
    /// Вкладка на месте, но плеер сайта сломался во время передачи (сессия 11a, решение 237):
    /// страница больше не источник. Для плеера это тот же конец «досмотреть скачанное», что и
    /// закрытая вкладка, только со своими словами. Появилось в протоколе 1.3
    /// ([`TabGoneReason::introduced_in`]); при общей версии ниже экстеншен шлёт
    /// [`Self::NavigatedAway`], а сторож плеера такое значение отвергает как нарушение.
    PagePlayerFailed,
}

impl TabGoneReason {
    /// В какой версии протокола появилась эта причина (`crate::version`, правило 5).
    ///
    /// Версии типов сообщений знают `PageSessionMessage::introduced_in` и общая таблица
    /// `fixtures/message-versions.json`; новое ЗНАЧЕНИЕ старого сообщения версионируется здесь
    /// (таблица значений — `fixtures/value-versions.json`). Зеркало — `tabGoneReasonIntroducedIn`
    /// (`introduced-in.ts`).
    #[must_use]
    pub const fn introduced_in(self) -> ProtocolVersion {
        match self {
            Self::Closed | Self::NavigatedAway | Self::BrowserClosing => ProtocolVersion::V1_0,
            Self::PagePlayerFailed => ProtocolVersion::V1_3,
        }
    }
}

/// Старшая из двух версий (одного `major`): версия сообщения со всеми его значениями.
const fn max_version(first: ProtocolVersion, second: ProtocolVersion) -> ProtocolVersion {
    if second.minor > first.minor {
        second
    } else {
        first
    }
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
#[serde(
    remote = "Self",
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
    /// В этом поколении вместо `MediaSource` у элемента файл `<video src>`.
    ///
    /// Байты файла плеер сам запросит командой `fetch_range`. Ни MIME, ни
    /// размера, ни адреса здесь нет: адресов у плеера нет вовсе, а размер
    /// придёт в `range_fetch_finished`. Взаимоисключимо с `source_buffer_added`
    /// в том же поколении (правила — в `crate::stream_gate`).
    FileSourceAdded {
        /// Поколение, чьё содержимое — файл.
        generation: StreamGeneration,
    },
    /// В этом поколении у элемента нативный HLS (`<video src=…m3u8>`), протокол 1.1.
    ///
    /// HLS ведёт плеер: каждый ресурс он сам попросит командой `fetch_resource`,
    /// начиная с корневого плейлиста `root`. Адресов здесь нет: фон экстеншена
    /// заменил их номерками (решение 228). Взаимоисключимо с `source_buffer_added`
    /// и `file_source_added` в том же поколении (правила — в `crate::stream_gate`).
    HlsSourceAdded {
        /// Поколение, чьё содержимое — HLS.
        generation: StreamGeneration,
        /// Номерок корневого плейлиста (мастер или сразу media-плейлист).
        root: ResourceHandle,
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
    /// Что с рекламой на странице изменилось (решения 184, 206).
    AdState {
        /// Новое состояние целиком.
        ad: PageAdState,
    },
    /// Запрос байтов `<video src>` закончен.
    RangeFetchFinished {
        /// Номер запроса из `fetch_range`.
        request_id: RangeRequestId,
        /// Итог.
        outcome: RangeFetchOutcome,
    },
    /// Запрос ресурса HLS закончен (протокол 1.1): все байты тела уже ушли
    /// рамками вида 2 с этим `request_id` (или запрос не выполнен).
    ResourceFetchFinished {
        /// Номер запроса из `fetch_resource`.
        request_id: RangeRequestId,
        /// Итог.
        outcome: ResourceFetchOutcome,
    },
    /// Вкладка больше не источник: плеер доигрывает скачанное (решение 42).
    TabGone {
        /// Почему.
        reason: TabGoneReason,
    },
    /// Страница больше не источник, но связь остаётся открытой (протокол 1.4, решение 240):
    /// экстеншен ждёт `reopen_page` и может вернуть страницу. В отличие от `tab_gone`
    /// сеанс НЕ кончается. Байты старого поколения после этого приходить не должны.
    PageLeft {
        /// Почему страница ушла (те же причины, что у `tab_gone`).
        reason: TabGoneReason,
    },
    /// Страница вернулась (протокол 1.4): тот же сеанс продолжается, дальше идёт новое
    /// поколение потока.
    PageReturned {},
    /// Вернуть страницу не вышло (протокол 1.4): после этого экстеншен закрывает
    /// соединение кодом 4002.
    PageReopenFailed {
        /// Почему не вышло.
        reason: PageReopenFailure,
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
            | Self::AdState { .. }
            | Self::RangeFetchFinished { .. }
            | Self::ResourceFetchFinished { .. }
            | Self::TabGone { .. }
            | Self::PageLeft { .. }
            | Self::PageReturned {}
            | Self::PageReopenFailed { .. }
            | Self::SeekStarted { .. } => StreamScope::Unbound,
            Self::GenerationStarted { generation }
            | Self::FileSourceAdded { generation }
            | Self::HlsSourceAdded { generation, .. }
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

    /// В какой версии протокола появился ТИП сообщения (`crate::version`, правило 5).
    ///
    /// Совпадает с общей таблицей `fixtures/message-versions.json`. Значения внутри типа
    /// могут быть новее (1.3: `tab_gone{page_player_failed}`) — версию самого сообщения со
    /// всеми значениями даёт [`Self::required_version`].
    #[must_use]
    pub const fn introduced_in(&self) -> ProtocolVersion {
        match self {
            Self::HlsSourceAdded { .. } | Self::ResourceFetchFinished { .. } => {
                ProtocolVersion::V1_1
            }
            Self::PageLeft { .. } | Self::PageReturned {} | Self::PageReopenFailed { .. } => {
                ProtocolVersion::V1_4
            }
            Self::Attach { .. }
            | Self::GenerationStarted { .. }
            | Self::FileSourceAdded { .. }
            | Self::SourceBufferAdded { .. }
            | Self::SourceBufferTypeChanged { .. }
            | Self::TimestampOffsetChanged { .. }
            | Self::SourceBufferRemoved { .. }
            | Self::SourceBufferAborted { .. }
            | Self::SeekStarted { .. }
            | Self::DurationChanged { .. }
            | Self::EndOfStream { .. }
            | Self::PagePlayback { .. }
            | Self::VariantMenuChanged { .. }
            | Self::AdState { .. }
            | Self::RangeFetchFinished { .. }
            | Self::TabGone { .. } => ProtocolVersion::V1_0,
        }
    }

    /// Самая старая общая версия сеанса, при которой страница вправе прислать именно это
    /// сообщение: самая новая из версий его типа ([`Self::introduced_in`]) и его значений.
    ///
    /// Сообщение, которого нет в общей версии, страница слать не вправе; плеер такое
    /// отвергает (`crate::stream_gate::StreamGate`) — «договорились — значит строго» и для
    /// значений (1.3: `tab_gone{page_player_failed}` при общей 1.2 — нарушение).
    #[must_use]
    pub const fn required_version(&self) -> ProtocolVersion {
        match self {
            // Тип `tab_gone` есть с 1.0, поэтому версия значения всегда не старше типа.
            Self::TabGone { reason } => reason.introduced_in(),
            // Тип `page_left` из 1.4, причина не новее его, но берём наибольшую явно.
            Self::PageLeft { reason } => {
                max_version(Self::introduced_in(self), reason.introduced_in())
            }
            other => other.introduced_in(),
        }
    }
}

impl ProtocolMessage for PageSessionMessage {
    const KNOWN_TYPES: &'static [&'static str] = &[
        "attach",
        "generation_started",
        "file_source_added",
        "hls_source_added",
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
        "ad_state",
        "range_fetch_finished",
        "resource_fetch_finished",
        "tab_gone",
        "page_left",
        "page_returned",
        "page_reopen_failed",
    ];

    fn validate(&self) -> Result<(), ValueViolation> {
        match self {
            Self::SourceBufferRemoved {
                start_us,
                end: RemovalEnd::At { time_us },
                ..
            } if time_us <= start_us => Err(ValueViolation::EmptyRange),
            Self::VariantMenuChanged { menu } => menu.validate(),
            Self::AdState { ad } => ad.validate(),
            _ => Ok(()),
        }
    }
}
