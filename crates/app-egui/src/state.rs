/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use fastiplayer_config::FrameServerLiveScrubDecodeModeConfig;
use fastiplayer_settings::{AppRouteApplyResult, MediaServiceRuntimeSettingsUpdate};
use player_core::{
    FrameCounters, MediaInstanceId, PlaybackRate, PlaybackState, PlayerCommand, PlayerEvent,
    PlayerRenderError, PlayerRuntimeApplyResult, PlayerRuntimeSettingsUpdate, PlayerSnapshot,
    PlayerVideoDecoderThreadConfig, PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent,
    PlayerWorkerShutdownDeadline, PlayerWorkerShutdownOutcome, QualitySelection, ScrubCommitPolicy,
    SeekRequest, VideoBackendSelectionRequest,
};
use render_core::RenderDiagnostics;
use render_shell_api::RenderableFrameKey;
use tracing::{debug, info, instrument, warn};
use video_present_core::{VideoFrameLease, VideoPresentFrameIdentity};
use winit::window::Window;

use crate::app_wake::AppWakePort;
use crate::dma_buf_runtime_fallback::{
    DmaBufFallbackTrigger, DmaBufRuntimeFallbackController, DmaBufRuntimeFallbackError,
    DmaBufRuntimeFallbackFailure, PendingDmaBufFallback,
};
use crate::local_file_open::{LocalFileOpenJob, LocalFileOpenRestoreOutcome, LocalFileOpenResult};
use crate::local_open_message::local_open_preparing_message;
use crate::settings_runtime::CommittedConfigSnapshot;
use crate::settings_ui::{SettingsUiAction, SettingsUiModel};
use crate::startup_readiness::{
    StartupReadinessAbortReason, StartupReadinessExpectation, StartupReadinessTracker,
};
use crate::telemetry::Telemetry;
use crate::ui::animation::AnimationState;
use crate::ui::player_controls::{self, ControlAction};
use crate::ui::sidebar::{self, SidebarHostState, SidebarRenderContext, SidebarWidthPoints};
use crate::ui::skin::{self, PlayerSkin};
use crate::ui::timeline::{
    self, TimelineAction, TimelineLiveScrubDecodeMode, TimelineLiveScrubSettingsSnapshot,
    TimelineUiState,
};
use crate::ui::titlebar_icon_area::TitlebarIconAreaAction;
use crate::ui::window_chrome::{self, WindowChromeInput, WindowChromeStyle};
use crate::video_pipeline_selector::{
    VideoBackendKind, VideoPipelinePlan, select_confirmed_software_fallback_plan,
    select_video_pipeline_plan,
};

mod audio_tracks;
mod automatic_web_media_quality;
mod dropped_subtitles;
mod external_open;
mod frame_context;
pub use frame_context::AppFrameContext;
mod fullscreen_chrome_routing;
mod language_label_disambiguation;
mod local_open_install;
mod main_visual_override;
mod media_jobs;
mod notification_routing;
mod notifications;
mod settings_media_rebuild;
pub(crate) use media_jobs::playback_intent_from_snapshot;
pub(crate) use notifications::{
    CenterNotice, NotificationId, NotificationsFrame, RetryOffer, ToastKind, ToastView,
};
pub(crate) use settings_media_rebuild::{
    RebuildPrepareRequest, RebuildSourceInput, SettingsMediaRebuildStart,
    media_reconfigure_install_failure, prepare_rebuild_source,
};
mod media_retry;
#[cfg(test)]
pub(crate) use media_retry::{
    LocalOpenSlot, MediaRetryPort, ReopenPosition, RetryStart, execute_media_retry,
};
pub(crate) use media_retry::{MediaRetryTarget, RetryPlan};
mod playlist_attachment;
mod playlist_transport;
mod position_wake;
mod preparation_cancel;
pub(crate) use playlist_transport::LifecycleTimelineSeekSettlement;
#[cfg(test)]
pub(crate) use playlist_transport::settle_timeline_seek_receipts_until;
mod present_frame_cache;
mod redraw_runtime;
mod renderer_follow_up;
pub(crate) use redraw_runtime::{RetainedUiGeometry, SettingsVisualFacts};
mod rendered_ui;
pub(crate) use rendered_ui::{AppUiRenderTimings, PlaylistUiFrameModels, RenderedAppUi};
mod same_item_candidate_switch;
mod sidebar_controller;
mod startup_context;
mod strong_media_open;
mod subtitle_appearance_edit;
mod subtitle_control_actions;
mod ui_frame_model;
pub(crate) use ui_frame_model::{PlaylistVisualKey, UiFrameModel};
pub(crate) use ui_textures::PreparedEguiContent;
mod subtitles;
mod suspended_media_resume;
#[cfg(test)]
pub(crate) use strong_media_open::PreparedPositionRestoreStrategy;
#[cfg(test)]
pub(crate) use strong_media_open::pending::{
    PostInstalledStartupPositionFacts, PostInstalledStartupPositionPlan,
    plan_post_installed_startup_position,
};
pub(crate) use strong_media_open::{
    InstalledSingleMediaOpen, PreparedSingleMediaOpen, StrongMediaOpenError, StrongMediaOpenPoll,
    StrongMediaOpenUserOutcome,
};
mod dynamic_decode;
pub(crate) use dynamic_decode::{RejectedSwitchImpact, classify_rejected_switch};
mod telemetry_panel;
mod timeline_inline_status;
mod track_memory;
mod ui_runtime;
mod ui_textures;
mod video_backend;
mod video_stream_requirement;
mod vod_endpoint_recovery;
mod volume_steps;
mod web_media_catalog;
mod window_frame_shape;

#[cfg(test)]
mod tests;

pub(crate) use audio_tracks::{AudioTrackMenuTarget, AudioTrackMenuView};
pub(crate) use main_visual_override::MainVisualOverrideAcquisition;
pub(crate) use media_jobs::ActiveMediaSource;
#[allow(unused_imports)]
pub use present_frame_cache::PresentFrameAcquisition;
pub use present_frame_cache::RenderablePresentFrame;
pub(crate) use startup_context::AppStateStartupContext;
use startup_context::{announce_renderer_selection, player_timeline_wake_bridge};
pub(crate) use subtitles::SubmittedFrameForSubtitles;
pub(crate) use subtitles::SubtitleMenuTrack;
pub(crate) use subtitles::{
    AppearanceEdit, AppearanceEditFinish, AppearanceEditView, PlacedLayerRect, SubtitleRgb,
    UserSubtitleStyle,
};
pub(crate) use subtitles::{
    AttachOutcome, SubtitleFileOrigin, SubtitleFileProblem, SubtitleMenuView,
    SubtitleTrackAvailability, SubtitleTrackLabel, SubtitleUnavailableReason, SubtitleVisibility,
    SyncNudge,
};
pub(crate) use track_memory::{
    TrackMemoryHandle, TrackMemoryShutdownOutcome, TrackMemoryStartOutcome, start_track_memory,
};
pub(crate) use video_backend::{
    BackendSwapVideoCheckpoint, BackendSwapVideoPhase, RendererVideoImportTarget,
    VideoPipelineRebuildError, VideoPipelineRebuildRequest,
};

use main_visual_override::MainVisualOverrideState;
use present_frame_cache::CachedRenderablePresentFrame;
use sidebar_controller::SidebarController;
pub(crate) use sidebar_controller::{
    ContentSlideDirection, SidebarContentTransition, SidebarSection,
};
use telemetry_panel::TelemetryPanelCache;
use timeline_inline_status::TimelineInlineStatusState;

/// Состояние приложения без владения playback pipeline.
pub struct AppState {
    /// Egui context — корневой объект egui для создания UI.
    pub egui_ctx: egui::Context,

    /// Egui_winit state — обработка ввода от winit.
    pub egui_winit_state: egui_winit::State,

    /// Текстуры интерфейса, ещё не доставленные в рендер, и запрос полного набора (02a).
    ui_textures: render_shell_api::UiTextureSync,

    /// Playback worker владеет `PlayerSession` и media pipeline на отдельном thread.
    pub player_worker: PlayerWorker,

    /// Счётчик кадров shell-анимации.
    pub frame_index: u64,

    /// Время запуска приложения для расчёта elapsed time.
    pub start_time: std::time::Instant,

    /// App-owned correlation реального process/media-open → surface + audio результата.
    startup_readiness: StartupReadinessTracker,

    /// Телеметрия — общие счётчики производительности.
    pub telemetry: Arc<Telemetry>,

    /// Read-only snapshot последнего committed config-а от authoritative settings runtime.
    committed_config_snapshot: CommittedConfigSnapshot,

    /// Read-only snapshot последнего capability report-а для app-owned selector-а.
    system_capabilities_snapshot: Option<SystemCapabilities>,

    /// Runtime-available audio decode snapshot без concrete registry types.
    audio_decode_capability_snapshot: audio::AudioDecodeCapabilitySnapshot,

    /// Exact renderer binding и immutable playlist view без controller ownership.
    playlist_attachment: Option<crate::playlist_runtime::PlaylistAppStateAttachment>,

    /// Viewport anchor принадлежит только Playlist UI и не следует за active media.
    playlist_ui_state: crate::ui::playlist::PlaylistUiState,

    /// Единственный владелец уведомлений: фатальная ошибка media, временные и инфо-toast-ы.
    notifications: notifications::NotificationCenter,

    /// Жест перетаскивания файлов в окно и hit-test панели плейлиста.
    external_open: crate::external_open::ExternalOpenOwner,

    /// Pending-состояние shell-слоя для операций, которые ещё не дошли до player.
    pub startup_pending: Option<String>,

    /// Последний snapshot, уже доставленный UI; используется shell redraw pacing-ом.
    last_player_snapshot: Arc<PlayerSnapshot>,

    /// Нужен один follow-up redraw после команды, которая уйдёт в worker асинхронно.
    pending_redraw_after_worker_command: bool,

    /// Последний кадр с уже полученными texture views для safe fallback на busy lock.
    cached_renderable_present_frame: Option<CachedRenderablePresentFrame>,

    /// App-owned main-video override во время scrub/seek visual pending state.
    main_visual_override_state: MainVisualOverrideState,

    /// Короткий inline статус timeline для scrub failures, owned только UI-слоем.
    timeline_inline_status: TimelineInlineStatusState,

    /// Класс активного video backend-а, чтобы не пересоздавать pipeline без нужды.
    current_video_backend_kind: Option<VideoBackendKind>,

    /// Exact identity renderer lifetime для app-side staged video candidate-а.
    renderer_generation: crate::video_pipeline_candidate::RendererGeneration,

    /// Отложенный запрос player-core на подбор backend-а под текущий стрим (`auto`).
    pending_video_backend_reselection: Option<VideoBackendSelectionRequest>,

    /// Requirement последнего активированного video-стрима, чтобы live-смена
    /// настроек (preference/decoder config) пересобирала pipeline с учётом того,
    /// какой именно кодек/профиль сейчас играет, а не выбирала backend вслепую.
    active_video_stream_requirement: video_stream_requirement::ActiveVideoStreamRequirement,

    /// Замороженный кадр прошлого backend-а на время живой смены backend/materializer:
    /// держится, пока worker не переключится и не выдаст первый кадр нового backend-а.
    backend_swap_frozen_frame: Option<CachedRenderablePresentFrame>,

    /// render generation на момент свапа; пока snapshot его не превысил, worker ещё не
    /// переключился, и кадры старого backend-а нельзя материализовать новым materializer-ом.
    backend_swap_from_generation: Option<u64>,
    /// Exactly-once policy state runtime DMA-BUF -> software fallback-а.
    dma_buf_runtime_fallback: DmaBufRuntimeFallbackController,
    /// Layout rejection, ожидающая обработки на app composition boundary.
    pending_dma_buf_fallback: Option<PendingDmaBufFallback>,

    /// Последний локальный файл, открытый shell-ом, для восстановления после suspend.
    current_local_file: Option<PathBuf>,

    /// Последний восстановимый media source intent для runtime source rebuild.
    active_media_source: Option<ActiveMediaSource>,
    /// App-owned exact binding и bounded attempt state для VOD signed-URL recovery.
    vod_endpoint_recovery: vod_endpoint_recovery::VodEndpointRecoveryRuntimeState,
    /// Renderer-bound receipts/candidate одной resume attempt; checkpoint остаётся в runtime.
    suspended_media_resume: Option<suspended_media_resume::SuspendedMediaResume>,

    /// Общий renderer-bound strong install (старт, плейлист, Open, VOD, переключение качества),
    /// который UI loop продвигает только неблокирующими шагами; владельцы различают свой request.
    pending_strong_media_open: Option<strong_media_open::PendingStrongMediaOpen>,

    /// S25/S36 transaction metadata поверх общего strong media-open envelope-а.
    same_item_switch: Option<same_item_candidate_switch::PendingSameItemSwitch>,

    /// Renderer-bound execution state UI playlist transport-а; traversal остаётся в runtime.
    playlist_transport: playlist_transport::PlaylistTransportRuntimeState,

    /// Активный async dialog/prepare job для локального файла.
    local_file_open_job: Option<LocalFileOpenJob>,
    /// Неблокирующая установка файла из Open после подготовки (`state/local_open_install.rs`).
    local_open_install: local_open_install::LocalOpenInstallOwner,
    /// Неблокирующая пересборка media после смены настроек (`state/settings_media_rebuild.rs`).
    settings_media_rebuild: settings_media_rebuild::SettingsMediaRebuildOwner,

    /// Process-lifetime owner wake port для каждого нового local-file job-а.
    local_file_open_wake_port: AppWakePort,

    /// Владелец субтитров текущего media (`state/subtitles.rs`): фоновая загрузка,
    /// активная дорожка и решение «что рисовать на этом кадре».
    subtitles: subtitles::SubtitleOwner,

    /// Память выбора дорожек «для каждого видео» (`state/track_memory`): общая для процесса.
    track_memory: TrackMemoryHandle,

    /// Диалог «Load file…» для субтитров (`subtitle_file_open.rs`): один диалог за раз.
    subtitle_file_picker: crate::subtitle_file_open::SubtitleFilePicker,

    /// Transient pointer state timeline; player position здесь не хранится.
    timeline_ui_state: TimelineUiState,

    /// Кэш строк telemetry panel; живёт в UI-слое и не владеет playback/render state.
    telemetry_panel_cache: TelemetryPanelCache,

    /// Отчёт выбора рендера при старте: строка телеметрии и источник семейства рендера.
    renderer_report: Option<crate::render_backend::RendererStartupReport>,

    /// Анимация выезда settings sidebar; runtime open-state остаётся целью перехода.
    sidebar_controller: SidebarController,
    /// Автоскрытие панелей и курсора в фуллскрине (`crate::fullscreen_chrome`).
    fullscreen_chrome: crate::fullscreen_chrome::FullscreenChromeController,
    /// Подсказка композитору о непрозрачности окна (`crate::window_surface_opacity`).
    surface_opacity: crate::window_surface_opacity::SurfaceOpacityDeclaration,

    /// Ephemeral pending/error state URL content; Installed source остаётся authoritative.
    url_sidebar_controller: crate::web_media_stream_model::UrlSidebarController,
    web_media_catalog_state: crate::web_media_catalog::WebMediaCatalogState,
    /// Hysteresis автоматического качества; catalog/player остаются read-only inputs.
    automatic_web_media_quality: automatic_web_media_quality::AutomaticWebMediaQualityController,
    pending_automatic_web_media_switch: Option<web_media_catalog::PendingAutomaticWebMediaSwitch>,
    web_media_fallback_notice: bool,
    /// Web-версия или язык звука из меню «♪»/клавиши `B`; запускается после egui pass-а.
    requested_web_audio_switch: Option<audio_tracks::WebAudioSwitchRequest>,

    /// Единственный владелец live geometry общей панели для всех sidebar sections.
    sidebar_host_state: SidebarHostState,

    /// Момент последнего advance анимации sidebar для вычисления дельты времени кадра.
    sidebar_slide_last_tick: Option<Instant>,

    /// Решение «надо ли рисовать»: триггеры, запросы egui, память последнего прохода (сессия 04).
    redraw: redraw_runtime::RedrawRuntime,
}

/// Кап дельты времени кадра для анимации sidebar: после паузы/лага кадров
/// анимация продолжается плавно, а не прыгает к концу.
const MAX_SIDEBAR_SLIDE_FRAME_DT_SECONDS: f32 = 0.1;

impl AppState {
    /// Создаёт новое состояние приложения и запускает playback worker.
    #[instrument(skip(
        window,
        telemetry,
        committed_config_snapshot,
        startup_context,
        local_file_open_wake_port,
        player_timeline_wake_port
    ))]
    pub fn new(
        window: &Window,
        startup_context: AppStateStartupContext,
        telemetry: Arc<Telemetry>,
        committed_config_snapshot: CommittedConfigSnapshot,
        audio_output_device_controller: audio::AudioOutputDeviceController,
        local_file_open_wake_port: AppWakePort,
        player_timeline_wake_port: AppWakePort,
    ) -> anyhow::Result<Self> {
        let egui_ctx = egui::Context::default();
        egui_ctx.set_theme(egui::Theme::Dark);
        crate::ui::egui_behavior::apply_app_egui_behavior(&egui_ctx);

        let viewport_id = egui_ctx.viewport_id();
        let egui_winit_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window,
            Some(window.scale_factor() as f32),
            Some(winit::window::Theme::Dark),
            None,
        );
        let audio_decoder_factory = Arc::new(audio::ProductionAudioDecoderFactory::default());
        let audio_decode_capability_snapshot =
            audio::AudioDecodeCapabilityProvider::audio_decode_capability_snapshot(
                audio_decoder_factory.as_ref(),
            );
        let redraw = redraw_runtime::RedrawRuntime::new(&egui_ctx, &player_timeline_wake_port);
        let timeline_activity_wake = player_timeline_wake_bridge(player_timeline_wake_port);
        let worker_config =
            PlayerWorkerConfig::from_app_config(committed_config_snapshot.as_config())
                .with_audio_decoder_factory(audio_decoder_factory)
                .with_audio_output_factory(Arc::new(audio::CpalAudioOutputFactory::new(
                    audio_output_device_controller,
                )))
                .with_audio_tempo_processor_factory(Arc::new(
                    audio_signalsmith::SignalsmithTempoProcessorFactory,
                ))
                .with_timeline_activity_wake(timeline_activity_wake)
                .with_presentation_wake(redraw.presentation_wake_bridge());
        let mut player_worker = PlayerWorker::spawn(worker_config)?;
        // Приёмник трубы встроенных субтитров: единственный потребитель — владелец субтитров.
        let subtitle_feed_receiver = player_worker.take_subtitle_feed_receiver();
        if let Err(error) = player_worker.try_send_command(PlayerCommand::SetVolume(
            committed_config_snapshot.remembered_audible_volume(),
        )) {
            warn!(error = %error, "Failed to apply the initial volume from config");
        }
        let sidebar_host_state =
            SidebarHostState::from_committed(committed_config_snapshot.sidebar_width_points());
        // Стартовые настройки субтитров берутся из committed config до его перемещения в состояние.
        let subtitles_config = committed_config_snapshot.as_config().subtitles.clone();
        let subtitles_network_wait =
            subtitles::network_reconnect_wait_of(committed_config_snapshot.as_config());

        let mut app_state = Self {
            egui_ctx,
            egui_winit_state,
            ui_textures: render_shell_api::UiTextureSync::new(),
            player_worker,
            frame_index: 0,
            start_time: std::time::Instant::now(),
            startup_readiness: StartupReadinessTracker::new(startup_context.process_started_at),
            telemetry,
            committed_config_snapshot,
            system_capabilities_snapshot: None,
            audio_decode_capability_snapshot,
            playlist_attachment: None,
            playlist_ui_state: crate::ui::playlist::PlaylistUiState::default(),
            notifications: notifications::NotificationCenter::with_startup_messages(
                startup_context.startup_error,
                startup_context.config_warning,
                Instant::now(),
            ),
            external_open: crate::external_open::ExternalOpenOwner::default(),
            startup_pending: None,
            last_player_snapshot: Arc::new(PlayerSnapshot::empty()),
            pending_redraw_after_worker_command: false,
            cached_renderable_present_frame: None,
            main_visual_override_state: MainVisualOverrideState::default(),
            timeline_inline_status: TimelineInlineStatusState::default(),
            current_video_backend_kind: None,
            renderer_generation: crate::video_pipeline_candidate::RendererGeneration::new_unique(),
            pending_video_backend_reselection: None,
            active_video_stream_requirement:
                video_stream_requirement::ActiveVideoStreamRequirement::default(),
            backend_swap_frozen_frame: None,
            backend_swap_from_generation: None,
            dma_buf_runtime_fallback: DmaBufRuntimeFallbackController::default(),
            pending_dma_buf_fallback: None,
            current_local_file: None,
            active_media_source: None,
            vod_endpoint_recovery: vod_endpoint_recovery::VodEndpointRecoveryRuntimeState::default(
            ),
            suspended_media_resume: None,
            pending_strong_media_open: None,
            same_item_switch: None,
            playlist_transport: playlist_transport::PlaylistTransportRuntimeState::default(),
            local_file_open_job: None,
            local_open_install: local_open_install::LocalOpenInstallOwner::default(),
            settings_media_rebuild: settings_media_rebuild::SettingsMediaRebuildOwner::default(),
            local_file_open_wake_port,
            subtitle_file_picker: crate::subtitle_file_open::SubtitleFilePicker::new(
                startup_context.subtitle_wake_port.clone(),
            ),
            subtitles: subtitles::SubtitleOwner::new(
                startup_context.subtitle_wake_port.clone(),
                subtitles::read_system_language(),
                &subtitles_config,
                subtitles_network_wait,
            ),
            track_memory: startup_context.track_memory.clone(),
            timeline_ui_state: TimelineUiState::default(),
            telemetry_panel_cache: TelemetryPanelCache::default(),
            renderer_report: startup_context.renderer_report.clone(),
            sidebar_controller: SidebarController::default(),
            fullscreen_chrome: crate::fullscreen_chrome::FullscreenChromeController::default(),
            surface_opacity:
                crate::window_surface_opacity::SurfaceOpacityDeclaration::for_transparent_window(),
            url_sidebar_controller: crate::web_media_stream_model::UrlSidebarController::default(),
            web_media_catalog_state: crate::web_media_catalog::WebMediaCatalogState::Inactive,
            automatic_web_media_quality:
                automatic_web_media_quality::AutomaticWebMediaQualityController::default(),
            pending_automatic_web_media_switch: None,
            web_media_fallback_notice: false,
            requested_web_audio_switch: None,
            sidebar_host_state,
            sidebar_slide_last_tick: None,
            redraw,
        };
        announce_renderer_selection(
            app_state.renderer_report.as_ref(),
            &mut app_state.notifications,
            Instant::now(),
        );
        match subtitle_feed_receiver {
            Some(receiver) => app_state.attach_subtitle_feed(receiver),
            None => {
                tracing::warn!("Subtitle feed receiver is unavailable; embedded subtitles are off")
            }
        }
        tracing::debug!(
            available_audio_decode_family_count = app_state
                .audio_decode_capability_snapshot()
                .available_families()
                .count(),
            "Composition retains immutable audio decode capability snapshot"
        );

        Ok(app_state)
    }

    /// Возвращает immutable runtime snapshot для app-owned web-media selection.
    #[must_use]
    pub const fn audio_decode_capability_snapshot(&self) -> audio::AudioDecodeCapabilitySnapshot {
        self.audio_decode_capability_snapshot
    }

    /// Закрывает renderer-bound player после process desktop admission shutdown-а в AppShell.
    pub(crate) fn shutdown_player_until(
        &mut self,
        deadline: crate::process_shutdown::ShutdownDeadline,
    ) -> PlayerWorkerShutdownOutcome {
        self.startup_readiness
            .abort_attempt(StartupReadinessAbortReason::Shutdown, Instant::now());
        self.player_worker
            .shutdown_before(PlayerWorkerShutdownDeadline::at(deadline.expires_at()))
    }

    /// Продвигает анимацию выезда settings sidebar к runtime open-state.
    ///
    /// Вызывается один раз за кадр до сборки settings UI model, чтобы видимость
    /// панели и video viewport считались по уже актуальной позиции анимации.
    pub(crate) fn advance_sidebar_slide(&mut self, settings_visible: bool, now: Instant) {
        self.sidebar_controller
            .reconcile_settings_visibility(settings_visible);
        let dt_seconds = self
            .sidebar_slide_last_tick
            .map(|last_tick| {
                now.saturating_duration_since(last_tick)
                    .as_secs_f32()
                    .min(MAX_SIDEBAR_SLIDE_FRAME_DT_SECONDS)
            })
            .unwrap_or(0.0);
        self.sidebar_slide_last_tick = Some(now);

        let duration_seconds = self
            .committed_config_snapshot
            .sidebar_slide_duration_seconds();
        self.sidebar_controller
            .advance(dt_seconds, duration_seconds);
    }

    /// `true`, пока анимация sidebar не достигла цели (нужен visual hold и repaint).
    #[must_use]
    pub(crate) fn sidebar_slide_is_animating(&self) -> bool {
        self.sidebar_controller.is_animating()
    }

    /// Обновляет read-only config snapshot из authoritative settings runtime.
    pub(crate) fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        let previous_sidebar_width = self.committed_config_snapshot.sidebar_width_points();
        let next_sidebar_width = snapshot.sidebar_width_points();
        let subtitles_changed =
            self.committed_config_snapshot.as_config().subtitles != snapshot.as_config().subtitles;
        let reconnect_wait_changed =
            self.committed_config_snapshot.as_config().network != snapshot.as_config().network;
        self.committed_config_snapshot = snapshot;
        if reconnect_wait_changed {
            self.apply_committed_network_wait_to_subtitles();
        }
        if subtitles_changed {
            // Настройки субтитров применяются здесь, а не в route: снимок обновляется только
            // после успешной транзакции, поэтому откат настроек не оставит новый стиль в плеере.
            self.apply_subtitles_config(
                &self.committed_config_snapshot.as_config().subtitles.clone(),
            );
        }
        self.sync_track_memory_capacity();
        if previous_sidebar_width != next_sidebar_width {
            self.sidebar_host_state
                .restore_committed_width(SidebarWidthPoints::from_committed(next_sidebar_width));
        }
        let live_scrub_settings = self.live_scrub_settings_snapshot();
        self.timeline_ui_state
            .defer_live_scrub_settings_change(live_scrub_settings);
    }

    /// Явно возвращает live host к последней сохранённой ширине после persistence failure.
    pub(crate) fn restore_sidebar_width(&mut self, width_points: SidebarWidthPoints) {
        self.sidebar_host_state
            .restore_committed_width(width_points);
    }

    /// Применяет player runtime settings через request/reply worker boundary.
    pub(crate) fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        self.player_worker.apply_runtime_settings(update)
    }

    /// Read-only preflight settings lifecycle boundary через app и worker owners.
    ///
    /// App-owned strong open/resume появляются раньше player command-а, поэтому проверяются
    /// здесь до worker query. Worker повторно валидирует собственный staged owner и закрывает
    /// очередь между этим preflight и фактическим settings rebuild.
    pub(crate) fn runtime_reconfigure_boundary_activity(
        &self,
    ) -> Result<
        Option<player_core::PlayerRuntimeBoundaryActivity>,
        player_core::PlayerRuntimeApplyError,
    > {
        if self.has_pending_prepared_media_strong()
            || self.has_pending_suspended_media_resume()
            || self.has_settings_media_rebuild()
        {
            return Ok(Some(
                player_core::PlayerRuntimeBoundaryActivity::PipelineLifecycle,
            ));
        }

        self.player_worker.runtime_reconfigure_boundary_activity()
    }

    /// Текущий decoder-thread config worker-а для app-owned pipeline rebuild.
    pub(crate) fn current_decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.player_worker.decoder_thread_config()
    }

    /// Возвращает committed backend policy для rebuild-ов, не вызванных её изменением.
    pub(crate) fn video_backend_preference(&self) -> fastiplayer_config::VideoBackendPreference {
        self.committed_config_snapshot.video_backend_preference()
    }

    /// Захватывает committed YtDlp policy для новой playlist metadata задачи.
    pub(crate) fn yt_dlp_metadata_config(&self) -> fastiplayer_config::YtDlpConfig {
        self.committed_config_snapshot.yt_dlp_metadata_config()
    }

    /// Клонирует committed config только для staged owner rebuild-а.
    pub(crate) fn committed_app_config(&self) -> fastiplayer_config::AppConfig {
        self.committed_config_snapshot.as_config().clone()
    }

    /// Применяет media/source policy через app-level owner.
    pub(crate) fn apply_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Applied
    }

    /// Инкрементирует счётчик кадров shell.
    #[inline]
    pub fn next_frame(&mut self) {
        self.frame_index = self.frame_index.saturating_add(1);
    }

    /// Собирает immutable context одного render frame-а.
    ///
    /// Внутри frame-а `PlayerSnapshot` читается только здесь, чтобы UI, renderer boundary
    /// и redraw pacing не расходились на один worker tick. Публикация в desktop integration
    /// остаётся отдельным side-effect boundary у вызывающего кода.
    #[must_use]
    pub fn begin_frame_context(
        &mut self,
        render_diagnostics: RenderDiagnostics,
    ) -> AppFrameContext {
        let player_snapshot = self.refresh_player_snapshot();
        self.startup_readiness
            .reconcile_tracks(&player_snapshot, Instant::now());

        AppFrameContext::new(
            player_snapshot,
            self.frame_counters_snapshot(),
            render_diagnostics,
        )
    }

    /// Фиксирует user-visible startup origin до начала media preparation/network path-а.
    pub(crate) fn begin_startup_readiness(&mut self, expectation: StartupReadinessExpectation) {
        self.startup_readiness
            .begin_attempt(expectation, Instant::now());
    }

    /// Уточняет consumer topology только по proof-у уже принятого prepared startup result-а.
    pub(crate) fn note_startup_prepared_consumer_proof(
        &mut self,
        proof: crate::startup_readiness::StartupPreparedConsumerProof,
    ) {
        self.startup_readiness
            .note_prepared_consumer_proof(proof, Instant::now());
    }

    /// Терминально закрывает попытку, которая больше не может показать startup result.
    pub(crate) fn abort_startup_readiness(&mut self, reason: StartupReadinessAbortReason) {
        self.startup_readiness.abort_attempt(reason, Instant::now());
    }

    /// Передаёт exact correlated player event app-owned readiness tracker-у.
    pub(crate) fn note_startup_player_event(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        event: &PlayerEvent,
    ) {
        self.startup_readiness
            .note_player_event(media_instance_id, event, Instant::now());
    }

    /// Фиксирует только кадр, который renderer уже действительно представил surface-у.
    pub(crate) fn note_startup_surface_frame_presented(
        &mut self,
        frame_identity: VideoPresentFrameIdentity,
    ) {
        self.startup_readiness.note_surface_frame_presented(
            self.last_player_snapshot.media_instance_id,
            frame_identity,
            self.last_player_snapshot.render_generation,
            Instant::now(),
        );
    }

    /// Показывает shell-level pending state, пока media ещё не передано в player.
    ///
    /// Начало нового открытия снимает прежнюю фатальную ошибку (решение владельца, сессия 04).
    pub fn set_startup_pending(&mut self, message: String) {
        self.notifications.resolve_media_failure();
        self.startup_pending = Some(message);
        self.mark_pending_worker_redraw();
    }

    /// Показывает shell-level ошибку, которая возникла до открытия media в player.
    ///
    /// Ошибка становится фатальной ошибкой центра: висит до ×, успешного или нового открытия.
    pub fn set_startup_error(&mut self, message: String) {
        self.report_open_failure(message, media_retry::RetryPlan::NotOffered);
    }

    /// Сбрасывает shell-level startup overlay после успешного открытия media.
    pub(super) fn clear_startup_status(&mut self) {
        self.startup_pending = None;
        self.notifications.resolve_media_failure();
        self.mark_pending_worker_redraw();
    }

    /// Обновляет кешированный read-only snapshot из `player-core` без внешней публикации.
    ///
    /// Это mutable boundary для worker snapshot: метод обновляет `last_player_snapshot`, но не
    /// выполняет desktop integration side effects. Снимок общий (`Arc`) с кэшем worker handle:
    /// повторный вызов без новой публикации worker-а отдаёт тот же `Arc` без глубокого клона.
    #[must_use]
    pub(crate) fn refresh_player_snapshot(&mut self) -> Arc<PlayerSnapshot> {
        let player_snapshot = self.player_worker.latest_snapshot();
        self.last_player_snapshot = Arc::clone(&player_snapshot);
        self.pump_embedded_subtitles();
        player_snapshot
    }

    /// Drain-ит worker snapshot только если wake действительно принёс новую live revision.
    pub(crate) fn refresh_player_snapshot_if_timeline_changed(
        &mut self,
    ) -> Option<Arc<PlayerSnapshot>> {
        let previous_identity = (
            self.last_player_snapshot.media_instance_id,
            self.last_player_snapshot.timeline.live_revision,
        );
        let player_snapshot = self.refresh_player_snapshot();
        let current_identity = (
            player_snapshot.media_instance_id,
            player_snapshot.timeline.live_revision,
        );
        (current_identity != previous_identity).then_some(player_snapshot)
    }

    /// Забирает одноразовый follow-up redraw после асинхронной worker command.
    pub fn take_pending_worker_redraw(&mut self) -> bool {
        std::mem::take(&mut self.pending_redraw_after_worker_command)
    }

    /// Помечает, что после текущего frame-а нужен ещё один redraw для worker response.
    pub(super) fn mark_pending_worker_redraw(&mut self) {
        self.pending_redraw_after_worker_command = true;
    }
}
