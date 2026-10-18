//! Контракт транспорта субтитров потока: HLS и DASH реализуют один и тот же трейт.
//!
//! Транспорт отвечает за сеть и разбор формата сегмента. Он отдаёт ленте события
//! УЖЕ на шкале плеера (или «ещё нельзя посчитать»), поэтому лента не знает ни про
//! `X-TIMESTAMP-MAP`, ни про периоды DASH.

use std::any::Any;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use source_core::CancellationToken;
use subtitle_core::{SubtitleEvent, SubtitleTime};

use super::error::StreamSubtitleError;
use super::model::{StreamSubtitleTrackId, StreamSubtitleTrackInfo};

/// Стабильный между перезагрузками индекса ключ сегмента (для HLS - media sequence).
///
/// Лента по нему отличает уже обработанные сегменты и забывает ушедшие из live-окна.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamSegmentKey(u64);

impl StreamSegmentKey {
    /// Создаёт ключ из стабильного номера сегмента.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Числовое значение ключа.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Opaque ссылка на сегмент: содержимое знает только выдавший её транспорт.
///
/// `Debug` ничего не раскрывает (внутри может быть секретный адрес).
#[derive(Clone)]
pub struct StreamSegmentRef(Arc<dyn Any + Send + Sync>);

impl StreamSegmentRef {
    /// Прячет данные транспорта за opaque ссылкой.
    #[must_use]
    pub fn new<T: Any + Send + Sync>(payload: T) -> Self {
        Self(Arc::new(payload))
    }

    /// Достаёт данные, если ссылку выдал транспорт с таким типом данных.
    #[must_use]
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl fmt::Debug for StreamSegmentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StreamSegmentRef(<opaque>)")
    }
}

/// Один сегмент индекса: ссылка и ПРИБЛИЗИТЕЛЬНОЕ положение внутри плейлиста.
///
/// `start` отсчитывается от начала плейлиста (сумма длительностей предыдущих сегментов),
/// а не от шкалы плеера: точное время появится только после `fetch_and_decode`.
#[derive(Clone, Debug)]
pub struct StreamSegment {
    /// Стабильный ключ сегмента.
    pub key: StreamSegmentKey,
    /// Opaque ссылка для `fetch_and_decode`.
    pub reference: StreamSegmentRef,
    /// Начало сегмента от начала плейлиста.
    pub start: Duration,
    /// Номинальная длительность сегмента.
    pub duration: Duration,
}

impl StreamSegment {
    /// Конец сегмента от начала плейлиста (насыщающее сложение).
    #[must_use]
    pub fn end(&self) -> Duration {
        self.start.saturating_add(self.duration)
    }
}

/// Нужно ли перечитывать индекс.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamIndexRefresh {
    /// Индекс завершён (VOD): сегменты не появятся.
    Never,
    /// Живой индекс: перечитать через указанное время.
    After(Duration),
}

/// Снимок индекса сегментов дорожки.
#[derive(Clone, Debug)]
pub struct StreamSegmentIndex {
    /// Сегменты в порядке возрастания `start`.
    pub segments: Vec<StreamSegment>,
    /// Расписание перечитывания.
    pub refresh: StreamIndexRefresh,
}

/// Поколение шкалы `StreamSegment::start` у транспорта.
///
/// Пока значение то же, `start` одного и того же сегмента между перечитываниями индекса не
/// меняется, и выученное лентой смещение «плейлист -> плеер» остаётся верным. Когда транспорт
/// вынужден закрепить шкалу заново (live-плейлист ушёл дальше окна перечитывания), значение
/// меняется, и лента обязана забыть выученное смещение.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamTimelineEpoch(u64);

impl StreamTimelineEpoch {
    /// Поколение с заданным номером.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Ограничения на скачивание и разбор одного сегмента (значения приходят из config).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSegmentLimits {
    /// Максимальный размер одного сегмента в байтах.
    pub max_bytes: NonZeroUsize,
}

/// Полуинтервал `[start, end)` на шкале плеера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerTimeSpan {
    /// Начало.
    pub start: SubtitleTime,
    /// Конец (не раньше начала).
    pub end: SubtitleTime,
}

/// Результат декодирования сегмента: события уже на шкале плеера.
#[derive(Clone, Debug)]
pub struct DecodedSegment {
    /// События сегмента (порядок не гарантируется).
    pub events: Vec<SubtitleEvent>,
    /// Участок шкалы плеера, который сегмент покрывает (реплики + номинальная длительность).
    pub covered: Option<PlayerTimeSpan>,
    /// Где на шкале плеера начинается сегмент, если транспорт это точно знает.
    ///
    /// Из разницы с `StreamSegment::start` лента учит смещение «плейлист -> плеер».
    pub segment_start: Option<SubtitleTime>,
}

/// Сегмент скачан и разобран, но шкала плеера для него ещё неизвестна.
///
/// Транспорт хранит разобранное внутри и досчитывает позже, когда демуксер видео
/// опубликует нужную точку отсчёта. Повторно сеть не трогается.
pub trait PendingSegmentDecode: Send {
    /// Пробует досчитать сегмент: `Ready` - готово, `Pending` - ещё рано (вернуть себя).
    fn try_resolve(self: Box<Self>) -> Result<SegmentDecode, StreamSubtitleError>;
}

/// Итог `fetch_and_decode`.
pub enum SegmentDecode {
    /// События посчитаны на шкале плеера.
    Ready(DecodedSegment),
    /// Шкала плеера для сегмента ещё неизвестна.
    Pending(Box<dyn PendingSegmentDecode>),
}

impl fmt::Debug for SegmentDecode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(decoded) => formatter
                .debug_struct("Ready")
                .field("events", &decoded.events.len())
                .field("covered", &decoded.covered)
                .finish(),
            Self::Pending(_) => formatter.write_str("Pending"),
        }
    }
}

/// Транспорт субтитров одного открытого потока (HLS, DASH).
///
/// Все методы блокирующие и зовутся только из фонового потока ленты. Токен отмены
/// проверяется до и после сети; уже идущий запрос он может не прервать.
pub trait StreamSubtitleTransport: Send + Sync {
    /// Безопасный список дорожек (без адресов).
    fn tracks(&self) -> Vec<StreamSubtitleTrackInfo>;

    /// Загружает свежий индекс сегментов дорожки.
    fn load_index(
        &self,
        track: StreamSubtitleTrackId,
        cancellation: &CancellationToken,
    ) -> Result<StreamSegmentIndex, StreamSubtitleError>;

    /// Поколение шкалы `start` для индекса, загруженного последним вызовом `load_index`.
    ///
    /// По умолчанию шкала никогда не закрепляется заново (DASH: `start` уже на шкале плеера,
    /// VOD: плейлист не меняется).
    fn index_timeline_epoch(&self, _track: StreamSubtitleTrackId) -> StreamTimelineEpoch {
        StreamTimelineEpoch::default()
    }

    /// Скачивает сегмент не больше `limits.max_bytes` и превращает его в события на шкале плеера.
    fn fetch_and_decode(
        &self,
        segment: &StreamSegment,
        limits: StreamSegmentLimits,
        cancellation: &CancellationToken,
    ) -> Result<SegmentDecode, StreamSubtitleError>;
}
