//! Субтитры DASH: дорожки текстовых AdaptationSet, индекс сегментов и скачивание документов.
//!
//! Этот слой знает про MPD, Period, PTO и сеть, но не знает про события субтитров: он отдаёт
//! байты документов (TTML либо WebVTT) и СДВИГ времени, который переводит время внутри
//! документов в шкалу плеера. Разбор документов и шкала событий - слой выше
//! (`media-source-open::stream_subtitles::dash`).
//!
//! Шкала плеера (как у видео): `player = presentation - source_origin`, где `presentation`
//! для ISOBMFF = `media - PTO + начало Period` (то же преобразование, что
//! `globalize_packet_timestamp` у A/V), а `source_origin` - ноль у VOD и начало сессии у live
//! (самая ранняя планируемая граница первого снимка, `DashLiveSessionTimeline`).
//!
//! Живой MPD: транспорт не ходит за манифестом сам. Его снимок (`DashTextPlanSet`) строится
//! тем же вызовом, что и снимок A/V, и подменяется вместе с ним при каждом принятом
//! обновлении; тут читается через [`DashTextSnapshotSource`].

mod fetch;
#[cfg(test)]
mod isolation_tests;
#[cfg(test)]
mod tests;
mod tracks;

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_mpd_core::DashTextKind;
use source_core::CancellationToken;
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use crate::plan::{DashPlannedResource, DashTextTimeBasis};
use crate::text_fmp4::TextTrackInit;

pub(crate) use tracks::{DashTextPlanSet, build_text_plan_set};
use tracks::{DashTextTrackDescription, DashTextTrackIdentity, describe_tracks};

/// Идентификатор дорожки внутри одного [`DashTextSubtitles`] (порядковый номер).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DashTextTrackId(u32);

impl DashTextTrackId {
    /// Идентификатор по порядковому номеру из списка `tracks()`; чужой номер даёт
    /// `UnknownTrack` при обращении.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// Порядковый номер дорожки в списке `tracks()`.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Безопасное описание дорожки: только метаданные (адресов нет).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTextTrackInfo {
    /// Идентификатор для `load_index`.
    pub id: DashTextTrackId,
    /// Тег языка из MPD.
    pub language: Option<Box<str>>,
    /// Название: `Label`, иначе по роли («Captions», «Forced», «Subtitles»), уникальное.
    pub name: Box<str>,
    /// Роль `forced-subtitle`.
    pub forced: bool,
    /// Роль `main` (либо первая обычная дорожка, если `main` нет ни у одной).
    pub default: bool,
}

/// Формат документа, который получил потребитель.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashTextDocumentFormat {
    /// TTML/IMSC.
    Ttml,
    /// WebVTT.
    WebVtt,
}

/// Один документ субтитров. Время внутри него ещё НЕ на шкале плеера.
#[derive(Clone, PartialEq, Eq)]
pub struct DashTextDocument {
    /// Формат разбора.
    pub format: DashTextDocumentFormat,
    /// Байты документа.
    pub bytes: Vec<u8>,
    /// Интервал сэмпла контейнера, из которого взят документ (`stpp`); `None` у отдельных
    /// файлов и `wvtt`. Документ действует только внутри интервала.
    pub sample_interval: Option<DashTextSampleInterval>,
}

/// Интервал `[start, end)` сэмпла на шкале времени самого документа (media timeline дорожки),
/// в микросекундах - до сдвига на шкалу плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashTextSampleInterval {
    /// Начало сэмпла.
    pub start_micros: i64,
    /// Конец сэмпла (строго позже начала).
    pub end_micros: i64,
}

impl fmt::Debug for DashTextDocument {
    /// Содержимое не печатается.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashTextDocument")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .field("sample_interval", &self.sample_interval)
            .finish()
    }
}

/// Результат скачивания сегмента: документы и сдвиг на шкалу плеера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTextFetchedSegment {
    /// Документы сегмента (у `stpp` по одному на сэмпл, у `wvtt` один на сегмент).
    pub documents: Vec<DashTextDocument>,
    /// Сколько микросекунд ПРИБАВИТЬ ко всем временам внутри документов, чтобы получить
    /// время на шкале плеера. Знаковое: бывает отрицательным (PTO больше начала Period).
    pub player_shift_micros: i64,
}

/// Когда перечитывать индекс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashTextRefresh {
    /// MPD статический: сегменты не появятся.
    Never,
    /// Живой MPD: перечитать через указанное время.
    After(Duration),
}

/// Сегмент индекса. Времена уже на шкале плеера (начало не раньше нуля).
#[derive(Clone)]
pub struct DashTextSegment {
    /// Стабильный между обновлениями ключ (начало на шкале презентации, микросекунды).
    pub key: u64,
    /// Начало на шкале плеера (насыщенно до нуля).
    pub start: Duration,
    /// Номинальная длительность.
    pub duration: Duration,
    /// Opaque ссылка для `fetch_segment`.
    pub handle: DashTextSegmentHandle,
}

impl fmt::Debug for DashTextSegment {
    /// Без адреса: ссылка непрозрачна.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashTextSegment")
            .field("key", &self.key)
            .field("start", &self.start)
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

/// Индекс сегментов дорожки.
#[derive(Debug, Clone)]
pub struct DashTextTrackIndex {
    /// Сегменты по возрастанию начала.
    pub segments: Vec<DashTextSegment>,
    /// Расписание перечитывания.
    pub refresh: DashTextRefresh,
}

/// Opaque ссылка на сегмент: внутри адрес, поэтому `Debug` ничего не раскрывает.
#[derive(Clone)]
pub struct DashTextSegmentHandle {
    inner: Arc<SegmentHandleInner>,
}

impl fmt::Debug for DashTextSegmentHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DashTextSegmentHandle(<opaque>)")
    }
}

/// Всё, что нужно для скачивания и привязки сегмента к шкале плеера.
struct SegmentHandleInner {
    kind: DashTextKind,
    basis: DashTextTimeBasis,
    /// Начало Period на шкале презентации.
    period_start: Duration,
    /// Начало сегмента от начала Period.
    local_start: Duration,
    resource: DashPlannedResource,
    initialization: Option<DashPlannedResource>,
    /// Ключ кэша init-сегмента: Period дорожки + отпечаток ресурса инициализации.
    init_key: InitCacheKey,
    /// (номер дорожки, начало Period в микросекундах): общий ключ «потока» внутри Period.
    period_key: PeriodKey,
    /// Начало шкалы плеера на шкале презентации (нулевое у VOD).
    source_origin: Duration,
}

/// Почему индекс или сегмент субтитров DASH получить не удалось. Без адресов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DashTextError {
    /// Дорожка не относится к этому транспорту.
    #[error("DASH subtitle track is not known to this transport")]
    UnknownTrack,
    /// Операцию отменили.
    #[error("DASH subtitle request was cancelled")]
    Cancelled,
    /// Ресурса нет (404/410 и прочие постоянные отказы).
    #[error("DASH subtitle resource was not found")]
    NotFound,
    /// Сеть либо сервер временно недоступны.
    #[error("DASH subtitle network request failed")]
    Network,
    /// Запрос отклонён политикой транспорта.
    #[error("DASH subtitle request was rejected by the transport policy")]
    Rejected,
    /// Ресурс больше лимита.
    #[error("DASH subtitle resource exceeds the {limit_bytes} byte limit")]
    TooLarge {
        /// Применённый лимит.
        limit_bytes: usize,
    },
    /// Данные (контейнер, время) разобрать нельзя.
    #[error("DASH subtitle data is malformed")]
    Malformed,
    /// Транспорт или поколение источника устарели.
    #[error("DASH subtitle transport or generation is stale")]
    Stale,
}

/// Текущее состояние источника: план, сеть и начало шкалы плеера.
pub(crate) struct DashTextSnapshot {
    pub(crate) plan: Arc<DashTextPlanSet>,
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) generation: SourceGeneration,
    /// Начало шкалы плеера на шкале презентации.
    pub(crate) source_origin: Duration,
    pub(crate) refresh: DashTextRefresh,
}

/// Откуда транспорт берёт актуальный снимок (статический или живой).
pub(crate) trait DashTextSnapshotSource: Send + Sync {
    /// Возвращает текущий снимок одним согласованным чтением.
    fn snapshot(&self) -> Result<DashTextSnapshot, DashTextError>;
}

/// Неизменяемый снимок статического MPD.
struct StaticTextSource {
    plan: Arc<DashTextPlanSet>,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
}

impl DashTextSnapshotSource for StaticTextSource {
    fn snapshot(&self) -> Result<DashTextSnapshot, DashTextError> {
        Ok(DashTextSnapshot {
            plan: Arc::clone(&self.plan),
            http: self.http.clone(),
            generation: self.generation,
            source_origin: Duration::ZERO,
            refresh: DashTextRefresh::Never,
        })
    }
}

/// Дорожки субтитров статического MPD (общий вход для VOD-открытий); `None` - дорожек нет.
pub(crate) fn static_text_subtitles(
    mpd: &dash_mpd_core::DashMpd,
    manifest_base: &source_core::HttpRequestTarget,
    http: &AdaptiveHttpContext,
    generation: SourceGeneration,
    maximum_segments: NonZeroUsize,
) -> Option<DashTextSubtitles> {
    let plan = build_text_plan_set(mpd, manifest_base, maximum_segments);
    DashTextSubtitles::for_static(plan, http.clone(), generation)
}

/// Предел кэша init-сегментов (структурная граница: Period в live идут по одному).
const MAX_CACHED_INITS: usize = 256;

/// Дорожки субтитров открытого DASH-источника.
///
/// Дёшево клонируется. Живёт вместе с источником: HTTP-контекст - клон контекста открытия
/// (те же сессия, политика редиректов и повторов, тот же токен отмены источника).
#[derive(Clone)]
pub struct DashTextSubtitles {
    shared: Arc<TextShared>,
}

struct TextShared {
    tracks: Vec<TrackEntry>,
    source: Box<dyn DashTextSnapshotSource>,
    /// Init-сегменты, уже скачанные для (дорожка, Period, идентичность ресурса init).
    inits: Mutex<std::collections::HashMap<InitCacheKey, TextTrackInit>>,
    /// Смещение нормализации SegmentList-дорожек: определяется первым скачанным сегментом
    /// Period и дальше используется всеми сегментами (как `NormalizeAtFirstPacket` у A/V).
    normalizations: Mutex<std::collections::HashMap<PeriodKey, i128>>,
}

/// (номер дорожки, начало Period в микросекундах).
type PeriodKey = (u32, u64);

/// Идентичность init-сегмента в кэше.
///
/// `initialization_fingerprint` - хеш (id Representation, адрес init, диапазон): живое
/// обновление MPD с другим init не должно получить устаревший. Сам адрес в ключ не кладём
/// (он секретный), хранится только необратимый отпечаток.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct InitCacheKey {
    period: PeriodKey,
    initialization_fingerprint: u64,
}

struct TrackEntry {
    identity: DashTextTrackIdentity,
    info: DashTextTrackInfo,
}

impl DashTextSubtitles {
    /// Дорожки статического MPD; `None`, если текстовых дорожек нет.
    pub(crate) fn for_static(
        plan: DashTextPlanSet,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
    ) -> Option<Self> {
        let plan = Arc::new(plan);
        Self::new(
            &plan,
            Box::new(StaticTextSource {
                plan: Arc::clone(&plan),
                // Вспомогательный вид: сбои субтитров не взводят endpoint recovery видео.
                http: http.for_auxiliary_resources(),
                generation,
            }),
        )
    }

    /// Дорожки по начальному плану и источнику снимков (live).
    pub(crate) fn new(
        initial_plan: &DashTextPlanSet,
        source: Box<dyn DashTextSnapshotSource>,
    ) -> Option<Self> {
        let descriptions = describe_tracks(initial_plan);
        let tracks: Vec<TrackEntry> = initial_plan
            .tracks
            .iter()
            .zip(descriptions)
            .enumerate()
            .map(|(index, (track, description))| TrackEntry {
                identity: track.identity.clone(),
                info: track_info(index, description),
            })
            .collect();
        if tracks.is_empty() {
            return None;
        }
        Some(Self {
            shared: Arc::new(TextShared {
                tracks,
                source,
                inits: Mutex::new(std::collections::HashMap::new()),
                normalizations: Mutex::new(std::collections::HashMap::new()),
            }),
        })
    }

    /// Дорожки в порядке MPD.
    #[must_use]
    pub fn tracks(&self) -> Vec<DashTextTrackInfo> {
        self.shared
            .tracks
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// Свежий индекс сегментов дорожки (для live - по текущему снимку MPD).
    ///
    /// Дорожка, исчезнувшая из свежего снимка, даёт пустой индекс.
    pub fn load_index(
        &self,
        track: DashTextTrackId,
        cancellation: &CancellationToken,
    ) -> Result<DashTextTrackIndex, DashTextError> {
        fetch::ensure_not_cancelled(cancellation)?;
        let entry = self.entry(track)?;
        let snapshot = self.shared.source.snapshot()?;
        let segments = snapshot
            .plan
            .find(&entry.identity)
            .map(|plan| fetch::index_segments(plan.periods.iter(), track.0, snapshot.source_origin))
            .unwrap_or_default();
        Ok(DashTextTrackIndex {
            segments,
            refresh: snapshot.refresh,
        })
    }

    /// Скачивает сегмент (не больше `max_bytes`) и превращает его в документы.
    pub fn fetch_segment(
        &self,
        segment: &DashTextSegmentHandle,
        max_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<DashTextFetchedSegment, DashTextError> {
        fetch::fetch_segment(&self.shared, &segment.inner, max_bytes, cancellation)
    }

    fn entry(&self, track: DashTextTrackId) -> Result<&TrackEntry, DashTextError> {
        usize::try_from(track.0)
            .ok()
            .and_then(|index| self.shared.tracks.get(index))
            .ok_or(DashTextError::UnknownTrack)
    }
}

impl fmt::Debug for DashTextSubtitles {
    /// Только число дорожек: адреса и сеть не печатаются.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashTextSubtitles")
            .field("tracks", &self.shared.tracks.len())
            .finish_non_exhaustive()
    }
}

fn track_info(index: usize, description: DashTextTrackDescription) -> DashTextTrackInfo {
    DashTextTrackInfo {
        // Число дорожек ограничено лимитами MPD; насыщение исключает панику.
        id: DashTextTrackId(u32::try_from(index).unwrap_or(u32::MAX)),
        language: description.language.map(String::into_boxed_str),
        name: description.name.into_boxed_str(),
        forced: description.forced,
        default: description.default,
    }
}
