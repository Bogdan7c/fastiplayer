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
//! - `layer_placement` (чистая геометрия) + `controls_obstruction` + `owner_placement`: куда сдвинуть
//!   готовый слой на экране (позиция пользователя, подъём над нижней панелью, чёрные полосы).
//!   Сдвиг уходит в рендер как `SubtitleLayerOffset` и меняет только uniform: без перерастеризации.
//! - `embedded_track` + `embedded_events` + `owner_embedded` + `embedded_fonts`: встроенные дорожки
//!   контейнера (сессия 06): список из снимка плеера, кэш реплик из трубы пакетов, вложенные
//!   шрифты. Подробности и инварианты — в документации `owner_embedded`.
//! - `owner_memory`: связь с памятью выбора «для каждого видео» (сессия 07): восстановление при
//!   старте media (`begin_media_restoring`) и выдача ТОЛЬКО действий пользователя
//!   (`take_memory_changes`). Сама память - `state/track_memory`.
//! - `settings` + `limits` + `user_style`: настройки из `[subtitles]` конфига.
//! - `appearance_edit` + `owner_appearance_edit` + `sample_cue` + `owner_fonts_bootstrap`: правка
//!   внешнего вида на видео (черновик вживую, образец реплики, отмена/сохранение).
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
mod appearance_edit;
mod auto_select;
mod container_start;
mod control_actions;
mod controls_obstruction;
mod embedded_events;
mod embedded_fonts;
mod embedded_track;
mod file_loader;
mod intents;
mod job_runner;
mod layer_placement;
mod limits;
mod media_state;
mod menu_view;
mod owner_appearance_edit;
mod owner_container_start;
mod owner_embedded;
mod owner_fonts_bootstrap;
mod owner_intents;
mod owner_loading;
mod owner_memory;
mod owner_placement;
mod owner_render;
mod owner_stream;
mod owner_web;
mod presentation;
mod render_worker;
mod sample_cue;
mod settings;
mod sidecar_job;
mod stream_position;
mod stream_track;
mod sync_offset;
mod system_language;
#[cfg(test)]
mod tests;
mod track_list;
mod user_file_failures;
mod user_style;
mod web_download;
mod web_load_job;
mod web_track;

use std::path::PathBuf;
use std::time::Duration;

use media_source_open::stream_subtitles::StreamSubtitleSource;
use media_source_open::web_subtitle_fetch::WebSubtitleFetchError;
use player_core::MediaInstanceId;
use subtitle_core::SubtitleLanguage;
use subtitle_raster::CueSourceId;
use subtitle_sidecar::SidecarSubtitleCandidate;

use self::active_track::ActiveSubtitleTrack;
use self::appearance_edit::AppearanceEditSession;
pub(crate) use self::appearance_edit::{
    AppearanceEdit, AppearanceEditBegin, AppearanceEditChange, AppearanceEditFinish,
    AppearanceEditOutcome, AppearanceEditView,
};
use self::auto_select::PreferredLanguages;
use self::container_start::ContainerStart;
pub(crate) use self::control_actions::ControlActionFeedback;
pub(crate) use self::controls_obstruction::BottomControlsObstruction;
use self::embedded_fonts::AttachedFontsState;
pub(crate) use self::intents::{AttachOutcome, SubtitleVisibility};
use self::job_runner::{SidecarJob, SidecarJobPoll};
pub(crate) use self::layer_placement::PlacedLayerRect;
use self::media_state::{DiscoveryState, MediaSubtitles};
pub(crate) use self::menu_view::{
    SubtitleFileOrigin, SubtitleFileProblem, SubtitleMenuTrack, SubtitleMenuView,
    SubtitleTrackAvailability, SubtitleTrackLabel, SubtitleUnavailableReason,
};
use self::owner_embedded::EmbeddedFeedState;
pub(crate) use self::owner_embedded::EmbeddedUpdate;
use self::owner_memory::PendingRememberedChoice;
pub(crate) use self::owner_memory::{SubtitleMemoryChanges, SubtitleRestoreRequest};
pub(crate) use self::owner_placement::PlacedSubtitleOverlay;
#[cfg(test)]
use self::presentation::PresentationStats;
pub(crate) use self::presentation::SubmittedFrameForSubtitles;
use self::presentation::SubtitlePresenter;
use self::settings::SubtitleSettings;
pub(super) use self::settings::network_reconnect_wait_of;
#[cfg(test)]
use self::sidecar_job::SubtitleLoadFailure;
use self::sidecar_job::{
    FontLibraryPlan, PreloadTarget, SidecarJobOutcome, SidecarJobRequest, SidecarJobTask,
};
pub(crate) use self::sync_offset::SyncNudge;
pub(super) use self::system_language::read_system_language;
#[cfg(test)]
use self::track_list::TrackLoadState;
pub(crate) use self::user_style::{SubtitleRgb, UserSubtitleStyle};
pub(crate) use self::web_track::WebSubtitleSource;
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
    /// Web-видео с дорожками субтитров, которые скачиваются по требованию (YouTube).
    Web(WebSubtitleSource),
    /// Поток с рендициями субтитров (HLS): реплики подгружает фоновая лента вокруг позиции.
    Stream(StreamSubtitleSource),
    /// Оба списка сразу: субтитры extractor-а (yt-dlp) и рендиции потока.
    WebAndStream {
        /// Дорожки extractor-а.
        web: WebSubtitleSource,
        /// Рендиции потока.
        stream: StreamSubtitleSource,
    },
}

impl SubtitleMediaSource {
    /// Дорожки известны сразу с установкой media (без поиска файлов и без чтения пакетов).
    fn has_remote_tracks(&self) -> bool {
        matches!(
            self,
            Self::Web(_) | Self::Stream(_) | Self::WebAndStream { .. }
        )
    }
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
    /// `network.reconnect_wait_ms`: бюджет ожидания сети для ленты потоковых субтитров.
    /// Живёт отдельно от секции `[subtitles]`, поэтому `apply_config` берёт его отсюда.
    network_reconnect_wait: Duration,
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
    /// Сбои скачивания web-субтитров, ещё не показанные пользователю (по одному на попытку).
    download_failures: Vec<WebSubtitleFetchError>,
    /// Что сейчас закрывает низ видео (наблюдение UI на этом кадре).
    controls_obstruction: BottomControlsObstruction,
    /// Где слой оказался на экране при последнем размещении (для рамки правки на видео).
    last_placed_layer_rect: Option<PlacedLayerRect>,
    /// Картинка видео (пиксели окна) того же кадра: UI правки переводит мышь в доли картинки.
    /// Задаётся и сбрасывается вместе с `last_placed_layer_rect`.
    last_placed_picture_rect: Option<render_core::PictureRect>,
    /// Фактический сдвиг слоя того же кадра (после зажимов и подъёма над панелью): от него
    /// режим правки начинает перенос мышью. Задаётся и сбрасывается вместе с прямоугольником.
    last_placed_layer_offset: Option<render_shell_api::SubtitleLayerOffset>,
    /// Лимиты растеризатора изменились: пересоздать его при следующем media.
    rasterizer_reset_pending: bool,
    /// Сессия правки внешнего вида на видео (`None` - не правим).
    appearance_edit: Option<AppearanceEditSession>,
    /// Образец реплики для режима правки (`None`, если собрать не удалось).
    sample_track: Option<ActiveSubtitleTrack>,
    /// Стиль, который пользователь сохранил и который ждёт записи в конфиг.
    style_save_request: Option<UserSubtitleStyle>,
    /// Фоновое построение шрифтов для образца, когда растеризатора ещё нет.
    fonts_bootstrap_job: Option<SidecarJob>,
    /// Задачи, которые больше не нужны, но ещё работают: ждём их завершения без блокировки UI.
    retiring_jobs: Vec<SidecarJob>,
    source_ids: SourceIdAllocator,
    /// Чтение трубы встроенных дорожек: приёмник, буферы, счётчики.
    embedded_feed: EmbeddedFeedState,
    /// Вложенные шрифты текущего media (подготовка, ожидание растеризатора).
    attached_fonts: AttachedFontsState,
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
        network_reconnect_wait: Duration,
    ) -> Self {
        let settings = SubtitleSettings::from_config(config, network_reconnect_wait);
        let presenter_wake_port = wake_port.clone();
        Self {
            wake_port,
            presenter: SubtitlePresenter::new(
                system_language.clone(),
                &settings.style,
                settings.presenter_settings(),
                presenter_wake_port,
            ),
            default_visibility: settings.default_visibility,
            settings,
            network_reconnect_wait,
            system_language,
            media: None,
            queued_files: Vec::new(),
            download_failures: Vec::new(),
            enabled_preference_change: None,
            controls_obstruction: BottomControlsObstruction::Absent,
            last_placed_layer_rect: None,
            last_placed_picture_rect: None,
            last_placed_layer_offset: None,
            appearance_edit: None,
            sample_track: sample_cue::build_sample_track(),
            style_save_request: None,
            fonts_bootstrap_job: None,
            rasterizer_reset_pending: false,
            retiring_jobs: Vec::new(),
            source_ids: SourceIdAllocator { next_value: 0 },
            embedded_feed: EmbeddedFeedState::default(),
            attached_fonts: AttachedFontsState::default(),
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
    /// фоновый поиск, файлы из очереди становятся дорожками.
    ///
    /// Вариант без памяти выбора - для тестов; продукт вызывает из единой точки установки
    /// media [`Self::begin_media_restoring`].
    #[cfg(test)]
    pub(super) fn begin_media(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: SubtitleMediaSource,
    ) {
        self.begin_media_restoring(
            media_instance_id,
            source,
            SubtitleRestoreRequest::nothing_remembered(),
        );
    }

    /// То же, что `begin_media` (только в тестах), но с выбором «для этого видео» из памяти.
    ///
    /// Восстановление НЕ меняет глобальное «субтитры включены» (`default_visibility` и
    /// `enabled_preference_change` не трогаются) и не помечается как выбор пользователя:
    /// в память попадают только его собственные действия.
    pub(super) fn begin_media_restoring(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: SubtitleMediaSource,
        restore: SubtitleRestoreRequest,
    ) {
        self.reset_media();
        if std::mem::take(&mut self.rasterizer_reset_pending) {
            // Лимиты растеризатора изменились: новый растеризатор получит их вместе с
            // новой библиотекой шрифтов (строится в фоне первой задачей).
            self.presenter.discard_rasterizer();
            self.presenter
                .replace_settings(self.settings.presenter_settings());
        }

        let mut media = MediaSubtitles::new(media_instance_id, self.default_visibility);
        if matches!(source, SubtitleMediaSource::LocalFile(_)) {
            // Шкалу локального файла публикует демуксер контейнера: файлы рядом считают от нуля.
            media.container_start = ContainerStart::for_local_file();
        }
        self.adopt_queued_files(&mut media);
        owner_memory::apply_restore_request(&mut media, restore);
        let has_remote_tracks = source.has_remote_tracks();
        media.discovery = match source {
            SubtitleMediaSource::LocalFile(video_path) => {
                self.start_discovery(Some(video_path), &media)
            }
            // Дорожки web и потока известны сразу: ставим их в список, поиск файлов нужен только
            // если из памяти ждём файл, который пользователь когда-то добавил сам.
            SubtitleMediaSource::Web(web) => {
                self.install_remote_sources(&mut media, Some(&web), None)
            }
            SubtitleMediaSource::Stream(stream) => {
                self.install_remote_sources(&mut media, None, Some(stream))
            }
            SubtitleMediaSource::WebAndStream { web, stream } => {
                self.install_remote_sources(&mut media, Some(&web), Some(stream))
            }
            // Не локальный источник: искать рядом нечего, но запомненный файл надо проверить.
            SubtitleMediaSource::NotLocal if media.pending_memory.is_some() => {
                self.start_discovery(None, &media)
            }
            SubtitleMediaSource::NotLocal => DiscoveryState::Finished,
        };
        self.media = Some(media);
        // Труба могла принести сообщения этого media раньше, чем пришла установка.
        self.adopt_media_for_embedded_feed(media_instance_id);
        if has_remote_tracks {
            // Список дорожек web и потока полный с самого начала: память выбора и автовыбор (ручная
            // важнее распознанной речи, автоперевод — никогда) решаются сразу.
            self.reselect_automatically();
        }
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
            // Файл принёс пользователь вместе с видео: это его выбор «для этого видео».
            media.note_user_choice();
            media.visibility = SubtitleVisibility::Shown;
            self.default_visibility = SubtitleVisibility::Shown;
            self.enabled_preference_change = Some(SubtitleVisibility::Shown);
        }
    }

    /// Запускает поиск рядом с видео. Ошибка запуска потока — типизированная причина.
    fn start_discovery(
        &self,
        video_path: Option<PathBuf>,
        media: &MediaSubtitles,
    ) -> DiscoveryState {
        let preload = if media.user_chose_track || media.pending_memory.is_some() {
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
                remembered_file: media
                    .pending_memory
                    .as_ref()
                    .and_then(PendingRememberedChoice::file_to_check),
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
    ///
    /// Идущая правка внешнего вида при этом сохраняется (неявный выход, решение владельца 6):
    /// стиль попадает в очередь записи, которую забирает `AppState`.
    pub(super) fn clear_media(&mut self) {
        self.reset_media();
        self.end_appearance_edit_for_cleared_media();
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
        // Зеркала дорожек прежнего media в потоке-рисовальщике больше не нужны. Отпускаем их
        // ПЕРВЫМИ: инвариант «у потока не бывает цели без зеркала». Команды, которые поток
        // воспринимает как «перерисуй» (снятие шрифтов обнуляет его запись о выданном),
        // иначе могли бы застать его с живой целью и зеркалом прежнего media: он успел бы
        // нарисовать мёртвую дорожку (гонка, видимая как лишний вызов растеризатора).
        self.presenter.release_mirrored_tracks();
        // Шрифты и кэши встроенных дорожек принадлежат прежнему media (кэши реплик лежат в
        // `MediaSubtitles` и уходят вместе с ним).
        self.forget_attached_fonts();
        self.presenter.forget_shown_overlay();
    }

    /// Неблокирующий опрос задач. `true` — видимое состояние изменилось (нужна перерисовка).
    ///
    /// Всегда опустошает ящики (в том числе у уходящих задач), иначе wake-порт остался бы
    /// «взведённым» и следующие результаты не будили бы окно.
    pub(super) fn poll_jobs(&mut self) -> bool {
        let fonts_changed = self.poll_fonts_bootstrap();
        let attached_fonts_changed = self.poll_attached_fonts();
        let media_changed = self.poll_media_jobs();
        let stream_changed = self.poll_stream_feed();
        // Поток-рисовальщик опрашивается ПОСЛЕДНИМ: остальные опросы сбрасывают «взведённость»
        // wake-порта, а события потока не должны потеряться за этим сбросом.
        let worker_changed = self.poll_render_worker();
        fonts_changed || attached_fonts_changed || media_changed || stream_changed || worker_changed
    }

    /// Опрос задач текущего media и уходящих задач (см. [`Self::poll_jobs`]).
    fn poll_media_jobs(&mut self) -> bool {
        // Результаты уходящих задач выбрасываем; завершённые потоки освобождаем.
        self.retiring_jobs.retain(|job| {
            let _discarded = job.poll();
            !job.is_finished()
        });

        let Some(media) = &self.media else {
            if !self.has_running_font_jobs() {
                self.wake_port.acknowledge_abandoned_mailbox();
            }
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
        if !media.has_running_jobs() && !self.has_running_font_jobs() {
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

    /// Применяет новый `network.reconnect_wait_ms`: действует на ленты, запущенные после этого.
    pub(super) fn apply_network_reconnect_wait(&mut self, network_reconnect_wait: Duration) {
        self.network_reconnect_wait = network_reconnect_wait;
        self.settings.stream_limits.feed.outage_budget = network_reconnect_wait;
    }

    /// Применяет новую секцию `[subtitles]` после committed изменения настроек.
    ///
    /// - стиль действует сразу: следующий кадр растеризуется заново;
    /// - лимиты загрузки действуют со следующей задачи, лимиты растеризатора — с созданием
    ///   нового (растеризатор пересоздаётся при следующем media);
    /// - «включено» и список языков действуют со следующего media: текущий выбор
    ///   пользователя настройки не перебивают.
    pub(super) fn apply_config(&mut self, config: &fastiplayer_config::SubtitlesConfig) {
        let next = SubtitleSettings::from_config(config, self.network_reconnect_wait);
        if next == self.settings {
            return;
        }
        if next.presenter_settings() != self.settings.presenter_settings() {
            // Лимиты растеризатора и размеры подготовки наперёд действуют с созданием нового
            // потока-рисовальщика (при следующем media).
            self.rasterizer_reset_pending = true;
            self.settings.raster_limits = next.raster_limits;
            self.settings.prerender_frames = next.prerender_frames;
            self.settings.prepared_overlays = next.prepared_overlays;
        }
        self.default_visibility = next.default_visibility;
        self.settings = next;
        // Во время правки показ определяет черновик пользователя: его чужой коммит не
        // перезаписывает, обновляется только `before` (куда вернёт «Отмена»). Вне правки
        // новый стиль применяется сразу; презентатор сам пропускает неизменный стиль.
        self.follow_committed_style_in_session();
        self.sync_presenter_appearance();
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
    fn rasterizer_statistics(&mut self) -> Option<subtitle_raster::RasterStatistics> {
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
        ActiveMediaSource::Web(intent) => stream_track::subtitle_source_of_web_intent(intent),
        // У потока страницы нет ни папки с сайдкарами, ни адреса: встроенные дорожки
        // приходят в самом потоке.
        ActiveMediaSource::PlaybackWindow { .. } | ActiveMediaSource::BrowserStream => {
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
