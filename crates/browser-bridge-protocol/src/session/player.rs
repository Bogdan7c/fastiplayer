//! Текстовые сообщения сеанса от плеера к экстеншену (странице).
//!
//! Часы ведёт плеер (`results/00c.md`, п. 4): экстеншен получает позицию,
//! паузу, скорость, «жду данных» и «конец» и сам генерирует события
//! муляжа видеоэлемента. Тики идут по этому соединению, а не таймерами
//! страницы: в фоне Chromium будит их раз в минуту.

use serde::{Deserialize, Serialize};

use crate::codec::ProtocolMessage;
use crate::error::ValueViolation;
use crate::resource::{ResourceByteRange, ResourceFetchLength, ResourceHandle};
use crate::session::clock_generation::CLOCK_GENERATION_INTRODUCED_IN;
use crate::session::live_window::LiveSeekable;
use crate::session::page_subtitles::{SUBTITLE_CHOICE_INTRODUCED_IN, SubtitleChoice};
use crate::session::variants::VariantKind;
use crate::stream::{RangeRequestId, SeekId, StreamGeneration};
use crate::strict_shape::{strict_label, strict_object};
use crate::text::VariantId;
use crate::time::{ByteOffset, MediaTime, PlaybackRate, RangeFetchLength};
use crate::version::ProtocolVersion;

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(PlayerSessionMessage);
strict_label!(ClockState, BufferIntake, ResumeState, StreamRejection);

/// Номер тика часов: страница по нему отбрасывает запоздавшие тики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClockSequence(pub u32);

/// Что сейчас делает плеер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
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
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum BufferIntake {
    /// Места хватает.
    Accepting,
    /// Буфер полон: экстеншен придерживает рамки, а плеер страницы и сам
    /// перестаёт качать, когда его цель буфера достигнута.
    Full,
}

/// В каком состоянии вернуть видео на страницу (решение 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum ResumeState {
    /// Страница играет дальше (кнопка «Return»).
    Playing,
    /// Страница на паузе (закрыли плеер, заменили из другой вкладки).
    Paused,
}

/// Почему плеер не будет играть это поколение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
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
    /// Плеер не смог установить или играть это поколение по своей вине (старт
    /// декодера, прерванная установка, сбой воспроизведения), хотя сами байты
    /// были приемлемы (решение 128). Экстеншен вправе забрать видео обратно
    /// на страницу — это делается позже (сессия 11).
    PlaybackFailed,
}

/// Текстовое сообщение сеанса от плеера к экстеншену.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields
)]
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
        /// Поколение, которое описывает этот тик (протокол 1.10, решение 311; договор —
        /// [`crate::session::clock_generation`]).
        ///
        /// Нужно тени страницы, чтобы отличить часы прошлого видео (плеер ещё доигрывает или
        /// держит старое поколение) от часов текущего. `None` — ключа на проводе нет: общая
        /// версия ниже 1.10 или поколения у плеера ещё нет. Поле последнее, `null` отвергается.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::strict_shape::optional_field"
        )]
        generation: Option<StreamGeneration>,
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
    /// Зритель нажал «Skip»: нажать настоящую кнопку пропуска на странице (решение 184).
    SkipAd {},
    /// Дай байты файла `<video src>` (качает background экстеншена).
    FetchRange {
        /// Номер запроса.
        request_id: RangeRequestId,
        /// С какого байта.
        offset: ByteOffset,
        /// Сколько байт.
        length: RangeFetchLength,
    },
    /// Дай ресурс HLS по номерку (протокол 1.1, решение 228; качает фон экстеншена).
    ///
    /// Номер запроса — из того же счётчика, что у `fetch_range`. Плеер держит не
    /// больше [`crate::limits::MAX_CONCURRENT_RESOURCE_FETCHES`] незавершённых
    /// запросов ресурсов. Ответ — рамки вида 2 со смещениями внутри тела с 0 и
    /// `resource_fetch_finished`.
    FetchResource {
        /// Номер запроса.
        request_id: RangeRequestId,
        /// Номерок ресурса из переписанного плейлиста (или `root` поколения).
        resource: ResourceHandle,
        /// Весь ресурс или кусок.
        byte_range: ResourceByteRange,
        /// Потолок тела ответа: длиннее — `too_large`.
        max_bytes: ResourceFetchLength,
    },
    /// Окно эфира live HLS (протокол 1.2, решение 232): край и честное окно перемотки.
    ///
    /// Всё во времени плеера (как `clock.position_us`). Инвариант
    /// `start_us ≤ end_us ≤ edge_us` проверяет [`LiveSeekable::check_within_edge`]
    /// при разборе и при записи. Плеер шлёт окно только собеседнику с общей версией
    /// не ниже 1.2, только для live-поколения и при изменении окна — не чаще
    /// [`crate::limits::MIN_LIVE_WINDOW_INTERVAL_MS`].
    LiveWindow {
        /// Поколение, к которому относится окно (как у `buffer_state`).
        generation: StreamGeneration,
        /// Край эфира: самое новое время, которое есть у плеера.
        edge_us: MediaTime,
        /// Куда можно перемотать.
        seekable: LiveSeekable,
    },
    /// Зритель нажал play, пока страница ушла (протокол 1.4, решение 241): вернуть страницу
    /// на этой секунде. Позиция — во времени плеера (как `clock.position_us`). Ответ —
    /// `page_returned` или `page_reopen_failed`.
    ReopenPage {
        /// С какой секунды продолжить.
        position_us: MediaTime,
    },
    /// Зритель хочет увидеть страницу (протокол 1.6, решение 278), например сменить там
    /// качество. Экстеншен делает вкладку сеанса активной, просит её окно выйти вперёд и
    /// подсвечивает окно. Ответа нет; видео остаётся у плеера, сеанс не меняется. Плеер шлёт
    /// это только собеседнику с общей версией не ниже 1.6.
    ShowPage {},
    /// Зритель сам выбрал субтитры (протокол 1.7, сессия 14a): меню CC, клавиши C/V. Не шлётся
    /// по автовыбору, подсказке страницы, своему файлу или встроенной дорожке — экстеншену
    /// запоминать нечего. Плеер шлёт это только собеседнику с общей версией не ниже 1.7.
    SubtitleChoice {
        /// Что выбрал зритель.
        choice: SubtitleChoice,
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
        "skip_ad",
        "fetch_range",
        "fetch_resource",
        "live_window",
        "reopen_page",
        "show_page",
        "subtitle_choice",
        "stream_rejected",
    ];

    fn validate(&self) -> Result<(), ValueViolation> {
        match self {
            // Кусок длиннее потолка ответа — запрос сам себе противоречит: такой
            // ответ фон не смог бы отдать ни при каком сервере.
            Self::FetchResource {
                byte_range: ResourceByteRange::Slice { length, .. },
                max_bytes,
                ..
            } if length > max_bytes => Err(ValueViolation::NumberOutOfRange),
            // Окно эфира: начало не позже конца, конец не дальше края.
            Self::LiveWindow {
                edge_us, seekable, ..
            } => seekable.check_within_edge(*edge_us),
            // Остальные правила выражены типами полей.
            _ => Ok(()),
        }
    }
}

impl PlayerSessionMessage {
    /// В какой версии протокола появилось сообщение (`crate::version`, правило 5).
    ///
    /// Плеер сверяет это с общей версией сеанса ДО отправки: при общей 1.0
    /// `fetch_resource` не уходит никогда, а при общей 1.1 — `live_window`.
    #[must_use]
    pub const fn introduced_in(&self) -> ProtocolVersion {
        match self {
            Self::SubtitleChoice { .. } => SUBTITLE_CHOICE_INTRODUCED_IN,
            Self::ShowPage {} => ProtocolVersion::V1_6,
            Self::ReopenPage { .. } => ProtocolVersion::V1_4,
            Self::LiveWindow { .. } => ProtocolVersion::V1_2,
            Self::FetchResource { .. } => ProtocolVersion::V1_1,
            Self::Attached {}
            | Self::Clock { .. }
            | Self::BufferState { .. }
            | Self::Play {}
            | Self::Pause {}
            | Self::Seek { .. }
            | Self::SetRate { .. }
            | Self::ReturnToPage { .. }
            | Self::SelectVariant { .. }
            | Self::NextEpisode {}
            | Self::SkipAd {}
            | Self::FetchRange { .. }
            | Self::StreamRejected { .. } => ProtocolVersion::V1_0,
        }
    }

    /// Самая старая общая версия сеанса, при которой плеер вправе послать именно это
    /// сообщение: самая новая из версий его типа ([`Self::introduced_in`]) и его полей.
    ///
    /// Зеркало `PageSessionMessage::required_version`. Новое необязательное поле старого
    /// сообщения (1.10: `clock.generation`) версионируется здесь: тик без номера — форма 1.0,
    /// с номером — 1.10. Совпадает с общими таблицами `fixtures/message-versions.json` и
    /// `fixtures/field-versions.json` (сверяет `tests/fixtures.rs`).
    #[must_use]
    pub const fn required_version(&self) -> ProtocolVersion {
        match self {
            // Тип `clock` из 1.0, поэтому версия поля всегда не старше типа.
            Self::Clock {
                generation: Some(_),
                ..
            } => CLOCK_GENERATION_INTRODUCED_IN,
            other => other.introduced_in(),
        }
    }
}
