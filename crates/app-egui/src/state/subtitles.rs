//! Владелец субтитров текущего media в `AppState`.
//!
//! # Архитектура
//! - `SubtitleOwner` (этот файл) владеет жизненным циклом: какое media сейчас, какие у него
//!   дорожки, что выбрано, включены ли субтитры и какой сдвиг. Он единственный решает, что
//!   показывать, и скрывает субтитры при любом сомнении о принадлежности кадра текущему media.
//! - `media_state`: всё состояние одного media (отбрасывается целиком при смене media).
//! - `track_list`: дорожки media (найденные рядом + добавленные пользователем) и состояние
//!   загрузки каждой; таймлайн читается лениво, только для показываемой дорожки.
//! - `auto_select`: чистый автовыбор дорожки (решение владельца 14).
//! - `sidecar_job` + `file_loader` + `job_runner`: фоновая задача «найти файлы рядом, выбрать,
//!   прочитать одну дорожку» либо «прочитать один файл». Привязана к `MediaInstanceId`.
//! - `owner_loading`: приём результатов задач и запуск ленивой загрузки.
//! - `owner_intents` + `app_state_api`: intent-API для UI (включить, выбрать, сдвиг, файл).
//! - `menu_view`/`intents`: типы ответов API и read-only снимок для меню.
//! - `active_track`: загруженный таймлайн + идентичность источника + сдвиг синхронизации.
//! - `presentation`: решение по кадру (реплики по pts кадра -> overlay растеризатора).
//! - `settings` + `limits` + `appearance_mapping`: настройки из `[subtitles]` конфига.
//!
//! # Точка запуска (единая для всех способов открыть файл)
//! Любой путь установки media (кнопка Open, drag&drop, playlist next, пересборка после
//! смены настроек, resume) заканчивается в `AppState::record_installed_media*`
//! (`state/media_jobs.rs`), где одновременно известны `MediaInstanceId` и источник.
//! Оттуда вызывается `start_subtitles_for_installed_media`. Сброс — в
//! `clear_released_installed_media_source` и `record_cleared_media_after_exact_reset`.
//!
//! # Правило «субтитры старого media не показываются ни кадра»
//! Overlay отдаётся только если одновременно:
//! 1. дорожка загружена для того же `MediaInstanceId`, что в текущем snapshot плеера;
//! 2. `render_generation` отправляемого кадра совпадает с `snapshot.render_generation`.
//!
//! Пункт 2 надёжен без мерцания в обычном воспроизведении: `acquire_present_frame_for_render`
//! уже отбрасывает lease с другим поколением, а кэшированные кадры проверяются против того же
//! snapshot, поэтому в штатной работе равенство выполняется всегда. А поколение растёт при
//! установке нового media (staged commit) и сбросе media, поэтому кадр из прошлого media
//! (например, кэш) не пройдёт проверку, даже если snapshot уже сменился. Snapshot читается
//! один раз в начале кадра, а lease берётся позже в том же кадре; рассинхрон возможен только
//! на границе смены media/backend, где скрытие и нужно.
//!
//! Кадр без identity (stale-lease во время seek) субтитры не получает вовсе: старая картинка
//! остаётся на экране без реплики, первый свежий кадр показывает реплику по своему pts.

mod active_track;
mod app_state_api;
mod appearance_mapping;
mod auto_select;
mod control_actions;
mod file_loader;
mod intents;
mod job_runner;
mod limits;
mod media_state;
mod menu_view;
mod owner_intents;
mod owner_loading;
mod presentation;
mod settings;
mod sidecar_job;
mod sync_offset;
mod system_language;
#[cfg(test)]
mod tests;
mod track_list;

use std::path::PathBuf;

use player_core::MediaInstanceId;
use subtitle_core::SubtitleLanguage;
use subtitle_raster::{CueSourceId, SubtitleOverlay};
use subtitle_sidecar::SidecarSubtitleCandidate;

use self::auto_select::PreferredLanguages;
pub(crate) use self::control_actions::ControlActionFeedback;
pub(crate) use self::intents::{AttachOutcome, SubtitleVisibility};
use self::job_runner::{SidecarJob, SidecarJobPoll};
use self::media_state::{DiscoveryState, MediaSubtitles};
#[cfg(test)]
pub(crate) use self::menu_view::SubtitleMenuTrack;
pub(crate) use self::menu_view::{
    SubtitleFileOrigin, SubtitleMenuView, SubtitleTrackAvailability, SubtitleTrackLabel,
    SubtitleUnavailableReason,
};
#[cfg(test)]
use self::presentation::PresentationStats;
pub(crate) use self::presentation::SubmittedFrameForSubtitles;
use self::presentation::SubtitlePresenter;
use self::settings::SubtitleSettings;
#[cfg(test)]
use self::sidecar_job::SubtitleLoadFailure;
use self::sidecar_job::{
    FontLibraryPlan, PreloadTarget, SidecarJobOutcome, SidecarJobRequest, SidecarJobTask,
};
pub(crate) use self::sync_offset::SyncNudge;
pub(super) use self::system_language::read_system_language;
use self::track_list::TrackLoadState;
use super::{ActiveMediaSource, InstalledSingleMediaOpen};
use crate::app_wake::AppWakePort;
use player_core::MediaInstallCompletion;

/// Откуда у только что установленного media могут быть субтитры.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SubtitleMediaSource {
    /// Локальный файл: рядом с ним ищем файлы субтитров.
    LocalFile(PathBuf),
    /// Не локальный источник (web и прочее): файлов рядом нет; доступны только файлы,
    /// которые пользователь добавит сам.
    NotLocal,
}

/// Какое media сейчас видит плеер: сверяется с владельцем перед показом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CurrentMediaForSubtitles {
    /// `media_instance_id` из текущего snapshot (`None` — media нет).
    pub(super) media_instance_id: Option<MediaInstanceId>,
    /// `render_generation` из текущего snapshot.
    pub(super) render_generation: u64,
}

/// Что сделали с готовым результатом фоновой задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobResultDisposition {
    /// Результат принят (видимое состояние могло измениться).
    Applied,
    /// Результат относится к другому media (или задача отменена) и выброшен.
    DroppedStale,
}

/// Чей это результат: поиска или загрузки конкретной дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobOrigin {
    /// Поиск файлов рядом с видео.
    Discovery,
    /// Загрузка одной дорожки.
    TrackLoad(subtitle_core::SubtitleTrackId),
}

/// Выдаёт уникальные идентификаторы источников реплик для кэша растеризатора.
///
/// Каждая загруженная дорожка получает новый идентификатор, поэтому кэш не спутает реплики
/// разных дорожек с одинаковыми `SubtitleEventId` (контракт `CueKey`).
struct SourceIdAllocator {
    next_value: u64,
}

impl SourceIdAllocator {
    fn allocate(&mut self) -> CueSourceId {
        let source_id = CueSourceId::new(self.next_value);
        self.next_value = self.next_value.saturating_add(1);
        source_id
    }
}

/// Краткий вид состояния для тестов и диагностики (без внутренних типов).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubtitleLifecycleView {
    /// Media нет.
    Idle,
    /// Идёт поиск или загрузка показываемой дорожки.
    Loading,
    /// Показываемая дорожка загружена.
    Ready,
    /// Показывать нечего: дорожек нет либо ни одна не выбрана.
    NoSubtitleFile,
    /// Поиск или показываемая дорожка недоступны по типизированной причине.
    Unavailable,
}

/// Владелец субтитров (см. документацию модуля).
pub(super) struct SubtitleOwner {
    wake_port: AppWakePort,
    presenter: SubtitlePresenter,
    /// Настройки из конфига; лимиты захватываются задачами при их запуске.
    settings: SubtitleSettings,
    /// Язык системы: запасной язык предпочтений и текста.
    system_language: Option<SubtitleLanguage>,
    /// Состояние текущего media; `None` — media нет.
    media: Option<MediaSubtitles>,
    /// Запомненное «включены ли субтитры»: стартовое значение для следующего media.
    default_visibility: SubtitleVisibility,
    /// Файлы, ожидающие следующего media (бросок «видео + субтитры» вместе).
    queued_files: Vec<SidecarSubtitleCandidate>,
    /// Непрочитанное изменение «включено» по воле пользователя (для сохранения в конфиг).
    enabled_preference_change: Option<SubtitleVisibility>,
    /// Лимиты растеризатора изменились: пересоздать его при следующем media.
    rasterizer_reset_pending: bool,
    /// Задачи, которые больше не нужны, но ещё работают: ждём их завершения без блокировки UI.
    retiring_jobs: Vec<SidecarJob>,
    source_ids: SourceIdAllocator,
    /// Какие шрифты строить новой задаче; тесты выбирают встроенные.
    font_plan_for_new_library: FontLibraryPlan,
}

impl SubtitleOwner {
    /// Создаёт владельца без media. `system_language` — язык системы, прочитанный один раз
    /// на границе сборки приложения ([`read_system_language`]). `config` — стартовая
    /// секция `[subtitles]` конфига; из неё берутся стартовое «включено», стиль и лимиты.
    pub(super) fn new(
        wake_port: AppWakePort,
        system_language: Option<SubtitleLanguage>,
        config: &fastiplayer_config::SubtitlesConfig,
    ) -> Self {
        let settings = SubtitleSettings::from_config(config);
        Self {
            wake_port,
            presenter: SubtitlePresenter::new(
                system_language.clone(),
                settings.appearance,
                settings.raster_limits,
            ),
            default_visibility: settings.default_visibility,
            settings,
            system_language,
            media: None,
            queued_files: Vec::new(),
            enabled_preference_change: None,
            rasterizer_reset_pending: false,
            retiring_jobs: Vec::new(),
            source_ids: SourceIdAllocator { next_value: 0 },
            font_plan_for_new_library: FontLibraryPlan::BuildWithSystemFonts,
        }
    }

    /// Предпочитаемые языки с учётом языка системы.
    fn preferred_languages(&self) -> PreferredLanguages {
        PreferredLanguages::resolve(
            &self.settings.preferred_language_codes,
            self.system_language.as_ref(),
        )
    }

    /// Новое media установлено: прежнее состояние сброшено, для локального файла стартует
    /// фоновый поиск, файлы из очереди становятся дорожками. Вызывается из единой точки
    /// установки media.
    pub(super) fn begin_media(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: SubtitleMediaSource,
    ) {
        self.reset_media();
        if std::mem::take(&mut self.rasterizer_reset_pending) {
            // Лимиты растеризатора изменились: новый растеризатор получит их вместе с
            // новой библиотекой шрифтов (строится в фоне первой задачей).
            self.presenter.discard_rasterizer();
            self.presenter
                .replace_raster_limits(self.settings.raster_limits);
        }

        let mut media = MediaSubtitles::new(media_instance_id, self.default_visibility);
        self.adopt_queued_files(&mut media);
        if let SubtitleMediaSource::LocalFile(video_path) = source {
            media.discovery = self.start_discovery(video_path, &media);
        }
        self.media = Some(media);
        // Дорожки из очереди (если есть) читаются сразу, остальные — по требованию.
        self.ensure_display_track_loading();
    }

    /// Файлы из очереди становятся дорожками нового media; первый выбирается и включает
    /// субтитры (пользователь сам принёс файл).
    fn adopt_queued_files(&mut self, media: &mut MediaSubtitles) {
        let queued = std::mem::take(&mut self.queued_files);
        for candidate in &queued {
            let track_id = media.tracks.push_chosen(candidate);
            if media.selected.is_none() {
                media.selected = Some(track_id);
            }
        }
        if !queued.is_empty() {
            media.user_chose_track = true;
            media.visibility = SubtitleVisibility::Shown;
            self.default_visibility = SubtitleVisibility::Shown;
            self.enabled_preference_change = Some(SubtitleVisibility::Shown);
        }
    }

    /// Запускает поиск рядом с видео. Ошибка запуска потока — типизированная причина.
    fn start_discovery(&self, video_path: PathBuf, media: &MediaSubtitles) -> DiscoveryState {
        let preload = if media.user_chose_track {
            // Выбор уже сделан пользователем: лишнюю дорожку не читаем.
            PreloadTarget::Nothing
        } else if media.visibility == SubtitleVisibility::Shown {
            PreloadTarget::Selected
        } else {
            PreloadTarget::ForcedFallback
        };
        let request = SidecarJobRequest {
            task: SidecarJobTask::DiscoverNextToVideo {
                video_path,
                preferred_languages: self.preferred_languages(),
                preload,
            },
            fonts: self.font_plan_for_next_job(),
            limits: self.settings.load_limits,
        };
        match SidecarJob::spawn(request, self.wake_port.clone()) {
            Ok(job) => DiscoveryState::Searching(job),
            Err(failure) => {
                tracing::warn!(%failure, "Subtitles are unavailable; playback continues");
                DiscoveryState::Failed(failure)
            }
        }
    }

    /// Нужна ли новой задаче библиотека шрифтов: только пока растеризатора ещё нет.
    fn font_plan_for_next_job(&self) -> FontLibraryPlan {
        if self.presenter.has_rasterizer() {
            FontLibraryPlan::ReuseExisting
        } else {
            self.font_plan_for_new_library
        }
    }

    /// Media исчезло (release/reset): состояние и задачи сброшены. Очередь файлов для
    /// СЛЕДУЮЩЕГО media при этом сохраняется (её ставят до открытия видео).
    pub(super) fn clear_media(&mut self) {
        self.reset_media();
    }

    /// Останавливает задачи (уходят в `retiring_jobs`) и забывает состояние media и overlay.
    fn reset_media(&mut self) {
        if let Some(media) = self.media.take() {
            if let DiscoveryState::Searching(job) = media.discovery {
                job.cancel();
                self.retiring_jobs.push(job);
            }
            if let Some(load) = media.load_job {
                load.job.cancel();
                self.retiring_jobs.push(load.job);
            }
        }
        self.presenter.forget_shown_overlay();
    }

    /// Неблокирующий опрос задач. `true` — видимое состояние изменилось (нужна перерисовка).
    ///
    /// Всегда опустошает ящики (в том числе у уходящих задач), иначе wake-порт остался бы
    /// «взведённым» и следующие результаты не будили бы окно.
    pub(super) fn poll_jobs(&mut self) -> bool {
        // Результаты уходящих задач выбрасываем; завершённые потоки освобождаем.
        self.retiring_jobs.retain(|job| {
            let _discarded = job.poll();
            !job.is_finished()
        });

        let Some(media) = &self.media else {
            self.wake_port.acknowledge_abandoned_mailbox();
            return false;
        };
        let media_instance_id = media.media_instance_id;
        let mut finished: Vec<(JobOrigin, SidecarJobOutcome)> = Vec::new();
        if let DiscoveryState::Searching(job) = &media.discovery
            && let SidecarJobPoll::Finished(outcome) = job.poll()
        {
            finished.push((JobOrigin::Discovery, outcome));
        }
        if let Some(load) = &media.load_job
            && let SidecarJobPoll::Finished(outcome) = load.job.poll()
        {
            finished.push((JobOrigin::TrackLoad(load.track_id), outcome));
        }
        if !media.has_running_jobs() {
            // Задач, которых ждёт владелец, нет: сбрасываем «взведённость» порта.
            self.wake_port.acknowledge_abandoned_mailbox();
        }

        let mut visible_state_changed = false;
        for (origin, outcome) in finished {
            visible_state_changed |= self.accept_job_outcome(media_instance_id, origin, outcome)
                == JobResultDisposition::Applied;
        }
        visible_state_changed
    }

    /// Overlay для кадра, который прямо сейчас уходит в renderer, или `None`.
    ///
    /// Реализует правило из документации модуля: другое media или другое поколение
    /// ресурсов рендера — субтитров нет.
    pub(super) fn overlay_for_frame(
        &mut self,
        current: CurrentMediaForSubtitles,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        let media = self.media.as_ref()?;
        if current.media_instance_id != Some(media.media_instance_id)
            || frame.render_generation() != current.render_generation
        {
            return None;
        }
        let entry = media.tracks.get(media.display_track_id()?)?;
        let TrackLoadState::Ready(track) = &entry.load else {
            return None;
        };
        self.presenter.overlay_for_frame(track, frame)
    }

    /// Применяет новую секцию `[subtitles]` после committed изменения настроек.
    ///
    /// - стиль действует сразу: следующий кадр растеризуется заново;
    /// - лимиты загрузки действуют со следующей задачи, лимиты растеризатора — с созданием
    ///   нового (растеризатор пересоздаётся при следующем media);
    /// - «включено» и список языков действуют со следующего media: текущий выбор
    ///   пользователя настройки не перебивают.
    pub(super) fn apply_config(&mut self, config: &fastiplayer_config::SubtitlesConfig) {
        let next = SubtitleSettings::from_config(config);
        if next == self.settings {
            return;
        }
        if next.appearance != self.settings.appearance {
            self.presenter.replace_appearance(next.appearance);
        }
        if next.raster_limits != self.settings.raster_limits {
            self.rasterizer_reset_pending = true;
            self.settings.raster_limits = next.raster_limits;
        }
        self.default_visibility = next.default_visibility;
        self.settings = next;
    }

    /// Счётчики презентации.
    #[cfg(test)]
    fn presentation_stats(&self) -> PresentationStats {
        self.presenter.stats()
    }

    /// Текущее состояние для тестов.
    #[cfg(test)]
    fn lifecycle_view(&self) -> SubtitleLifecycleView {
        let Some(media) = &self.media else {
            return SubtitleLifecycleView::Idle;
        };
        if matches!(media.discovery, DiscoveryState::Searching(_)) {
            return SubtitleLifecycleView::Loading;
        }
        let display = media
            .display_track_id()
            .and_then(|track_id| media.tracks.get(track_id));
        match display.map(|entry| &entry.load) {
            Some(TrackLoadState::Ready(_)) => SubtitleLifecycleView::Ready,
            Some(TrackLoadState::NotLoaded | TrackLoadState::Loading) => {
                SubtitleLifecycleView::Loading
            }
            Some(TrackLoadState::Unavailable(_)) => SubtitleLifecycleView::Unavailable,
            None if matches!(media.discovery, DiscoveryState::Failed(_)) => {
                SubtitleLifecycleView::Unavailable
            }
            None => SubtitleLifecycleView::NoSubtitleFile,
        }
    }

    /// Причина недоступности (поиска или показываемой дорожки) для тестов.
    #[cfg(test)]
    fn unavailable_failure(&self) -> Option<&SubtitleLoadFailure> {
        let media = self.media.as_ref()?;
        if let DiscoveryState::Failed(failure) = &media.discovery {
            return Some(failure);
        }
        let entry = media.tracks.get(media.display_track_id()?)?;
        match &entry.load {
            TrackLoadState::Unavailable(failure) => Some(failure),
            _ => None,
        }
    }

    /// Счётчики растеризатора для тестов.
    #[cfg(test)]
    fn rasterizer_statistics(&self) -> Option<subtitle_raster::RasterStatistics> {
        self.presenter.rasterizer_statistics()
    }

    /// Для тестов: строить только встроенные шрифты (быстро, не зависит от машины).
    #[cfg(test)]
    fn use_embedded_fonts_only(&mut self) {
        self.font_plan_for_new_library = FontLibraryPlan::BuildEmbeddedOnly;
    }

    /// Для тестов: замена стиля.
    #[cfg(test)]
    fn replace_appearance(&mut self, appearance: subtitle_raster::SubtitleAppearance) {
        self.presenter.replace_appearance(appearance);
    }

    /// Для тестов: сколько ушедших задач ещё ждёт завершения.
    #[cfg(test)]
    fn retiring_job_count(&self) -> usize {
        self.retiring_jobs.len()
    }
}

/// Какой источник subtitles-владельцу соответствует источнику media.
fn subtitle_source_of(source: &ActiveMediaSource) -> SubtitleMediaSource {
    match source.physical_source() {
        ActiveMediaSource::LocalFile(path) => SubtitleMediaSource::LocalFile(path.clone()),
        ActiveMediaSource::Web(_) | ActiveMediaSource::PlaybackWindow { .. } => {
            SubtitleMediaSource::NotLocal
        }
    }
}

/// `MediaInstanceId` из receipt-а установки (`None`, если receipt не `Installed`).
pub(super) fn installed_media_instance_id(
    installed: &InstalledSingleMediaOpen,
) -> Option<MediaInstanceId> {
    match installed.completion {
        MediaInstallCompletion::Installed {
            media_instance_id, ..
        } => Some(media_instance_id),
        MediaInstallCompletion::Failed { .. } | MediaInstallCompletion::Cancelled { .. } => None,
    }
}
