//! Активная дорожка субтитров текущего media: таймлайн, идентичность источника и сдвиг.

use std::time::Duration;

use subtitle_core::{
    SubtitleEvent, SubtitleLanguage, SubtitleSyncOffset, SubtitleTime, SubtitleTimeline,
    TimelineBuildReport,
};
use subtitle_raster::CueSourceId;

/// Дорожка, готовая к показу.
///
/// Владеет таймлайном и всем, что нужно для ответа «какие реплики видны на кадре с `pts`»:
/// - `source_id` меняется для каждой новой дорожки, поэтому кэш растеризатора не спутает
///   реплики разных дорожек с одинаковыми `SubtitleEventId` (контракт `CueKey`);
/// - `sync_offset` — единственная точка, через которую пользовательский сдвиг синхронизации
///   влияет на время показа (сессия 04 лишь меняет его значение);
/// - `language` — язык дорожки (из имени файла), если он известен: по нему растеризатор
///   выбирает региональную форму CJK. Он неизменен, а `source_id` уникален для дорожки, так
///   что презентатору достаточно сравнивать `source_id`, чтобы заметить смену языка.
pub(super) struct ActiveSubtitleTrack {
    source_id: CueSourceId,
    timeline: SubtitleTimeline,
    sync_offset: SubtitleSyncOffset,
    language: Option<SubtitleLanguage>,
}

impl ActiveSubtitleTrack {
    /// Собирает дорожку из готового таймлайна.
    pub(super) fn new(
        source_id: CueSourceId,
        timeline: SubtitleTimeline,
        sync_offset: SubtitleSyncOffset,
        language: Option<SubtitleLanguage>,
    ) -> Self {
        Self {
            source_id,
            timeline,
            sync_offset,
            language,
        }
    }

    /// Меняет сдвиг синхронизации (вызывает только список дорожек, чтобы сдвиг оставался
    /// общим для всех дорожек media).
    pub(super) fn set_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        self.sync_offset = offset;
    }

    /// Начинает кэш реплик заново под новым источником: меняется сценарий дорожки, а старые
    /// `CueKey` не вправе жить при новом содержимом (контракт «тот же ключ = то же содержимое»).
    pub(super) fn restart_with_source(&mut self, source_id: CueSourceId) {
        self.source_id = source_id;
        self.timeline = SubtitleTimeline::new();
    }

    /// Язык дорожки; `None` — дорожка его не объявляет (тогда решает язык системы).
    pub(super) fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    /// Идентичность источника реплик для кэша растеризатора.
    pub(super) fn source_id(&self) -> CueSourceId {
        self.source_id
    }

    /// Таймлайн реплик (только чтение).
    pub(super) fn timeline(&self) -> &SubtitleTimeline {
        &self.timeline
    }

    /// Дописывает реплики встроенной дорожки (пакеты приходят потоком, дорожка растёт).
    ///
    /// Идентификаторы событий — порядок вставки, поэтому добавление не меняет `CueKey`
    /// уже растеризованных реплик. Дубликаты (после seek назад демуксер перечитывает те же
    /// пакеты) таймлайн отбрасывает сам и считает в отчёте.
    pub(super) fn append_events(
        &mut self,
        events: impl IntoIterator<Item = SubtitleEvent>,
    ) -> TimelineBuildReport {
        self.timeline.extend(events)
    }

    /// Время, по которому нужно искать реплики для кадра с данным `pts`.
    ///
    /// `frame_pts` — позиция кадра на шкале исходного файла (та же шкала, что у демуксера,
    /// см. отчёт сессии 03 про происхождение pts). Сдвиг применяется ровно здесь.
    pub(super) fn lookup_time_for(&self, frame_pts: Duration) -> SubtitleTime {
        self.sync_offset
            .lookup_time_for(subtitle_time_from_pts(frame_pts))
    }
}

/// Переводит `pts` кадра в микросекунды модели субтитров; сверхбольшое значение
/// насыщается, а не оборачивается (реплики за пределами i64 мкс всё равно не существуют).
pub(super) fn subtitle_time_from_pts(frame_pts: Duration) -> SubtitleTime {
    let micros = i64::try_from(frame_pts.as_micros()).unwrap_or(i64::MAX);
    SubtitleTime::from_micros(micros)
}
