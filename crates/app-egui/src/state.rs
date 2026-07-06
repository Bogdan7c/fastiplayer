/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use animation_core::{Easing, SlideTransition};
use capability_core::SystemCapabilities;
use desktop_integration::{DesktopIntegration, DesktopIntegrationEvent};
use media_core::TrackKind;
use player_core::{
    FrameCounters, MediaOpenRequest, MediaSource, PlaybackRate, PlaybackState, PlayerCommand,
    PlayerError, PlayerErrorKind, PlayerEvent, PlayerRenderError, PlayerRuntimeApplyResult,
    PlayerRuntimeSettingsUpdate, PlayerSnapshot, PlayerVideoDecoderThreadConfig, PlayerWorker,
    PlayerWorkerConfig, PlayerWorkerEvent, PreparedMedia, QualitySelection, ScrubCommitPolicy,
    SeekRequest, VideoBackendSelectionRequest, VideoDecodeRequirement,
};
use render_core::RenderDiagnostics;
use render_wgpu_video::{
    DmaBufWgpuFrameMaterializer, HostPlanarWgpuFrameMaterializer, WgpuFrameTextureViewMaterializer,
    WgpuFrameTextureViews, wrap_video_backend_for_wgpu_submission,
};
use rustiplayer_config::{FrameServerLiveScrubDecodeModeConfig, PlayerDemuxConfig};
use rustiplayer_settings::{AppRouteApplyResult, MediaServiceRuntimeSettingsUpdate};
use tracing::{debug, info, instrument, warn};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_present_core::VideoFrameLease;
use video_vaapi::VaapiVideoBackendFactory;
use winit::window::Window;

use crate::local_file_open::{
    LocalFileOpenEvent, LocalFileOpenJob, LocalFileOpenResult, local_file_prepare_error_message,
    preparing_local_file_message,
};
use crate::local_media;
use crate::settings_runtime::CommittedConfigSnapshot;
use crate::settings_ui::{SettingsUiAction, SettingsUiModel};
use crate::telemetry::Telemetry;
use crate::ui::animation::AnimationState;
use crate::ui::player_controls::{self, ControlAction};
use crate::ui::sidebar::{self, AppSidebarContent};
use crate::ui::skin::{self, PlayerSkin};
use crate::ui::timeline::{
    self, TimelineAction, TimelineLiveScrubDecodeMode, TimelineLiveScrubSettingsSnapshot,
    TimelineUiState,
};
use crate::ui::titlebar_icon_area::TitlebarIconAreaAction;
use crate::ui::window_chrome::{self, WindowChromeAction, WindowChromeInput, WindowChromeStyle};
use crate::video_pipeline_selector::{
    VideoBackendKind, VideoPipelinePlan, select_video_pipeline_plan,
};

mod main_visual_override;
mod media_jobs;
mod present_frame_cache;
mod telemetry_panel;
mod timeline_inline_status;
mod ui_runtime;
mod video_backend;

#[cfg(test)]
mod tests;

pub(crate) use main_visual_override::MainVisualOverrideAcquisition;
pub(crate) use media_jobs::ActiveMediaSource;
#[allow(unused_imports)]
pub use present_frame_cache::PresentFrameAcquisition;
pub use present_frame_cache::RenderablePresentFrame;
pub(crate) use video_backend::BackendSwapVideoPhase;

use main_visual_override::MainVisualOverrideState;
use present_frame_cache::CachedRenderablePresentFrame;
use telemetry_panel::TelemetryPanelCache;
use timeline_inline_status::TimelineInlineStatusState;

/// Immutable данные, зафиксированные один раз для текущего render frame-а.
pub struct AppFrameContext {
    /// Snapshot player-а, общий для UI, renderer boundary, desktop integration и pacing.
    player_snapshot: PlayerSnapshot,

    /// Renderer-neutral diagnostics, которые UI показывает в этом же frame-е.
    render_diagnostics: RenderDiagnostics,
}

/// CPU timing внутренних частей `AppState::render_ui`.
///
/// Структура остаётся internal API `app-egui`: `frame_prepare` получает только
/// длительности и не знает деталей хранения UI-состояния.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AppUiRenderTimings {
    /// Полная длительность вызова `render_ui`.
    pub(crate) total: Duration,

    /// Подготовка snapshot-derived значений до входа в egui closure.
    pub(crate) pre_ui_setup: Duration,

    /// Полный `egui_ctx.run_ui`, включая все панели и layout.
    pub(crate) egui_run: Duration,

    /// Рендер верхней панели.
    pub(crate) top_bar: Duration,

    /// Рендер нижних controls и timeline.
    pub(crate) bottom_controls: Duration,

    /// Рендер telemetry панели, если она включена в config.
    pub(crate) telemetry_panel: Duration,

    /// Рендер центрального overlay.
    pub(crate) center_overlay: Duration,

    /// Применение UI actions после egui closure.
    pub(crate) post_ui_actions: Duration,

    /// Была ли telemetry панель включена в этом кадре.
    pub(crate) telemetry_panel_visible: bool,
}

/// Результат app-owned UI подготовки до platform output и tessellation.
pub(crate) struct RenderedAppUi {
    /// Полный output egui за текущий кадр.
    pub(crate) full_output: egui::FullOutput,

    /// Visual settings actions, которые shell передаст authoritative runtime owner-у.
    pub(crate) settings_actions: Vec<SettingsUiAction>,

    /// Window chrome actions, которые shell применит через winit boundary.
    pub(crate) window_chrome_actions: Vec<WindowChromeAction>,

    /// Область video underlay в egui points; overlay-панели её не уменьшают.
    pub(crate) video_viewport_rect: egui::Rect,

    /// UI-области, под которыми video pass не должен рисовать кадр.
    pub(crate) video_exclusion_rects: Vec<egui::Rect>,

    /// Timing внутренних участков `render_ui`.
    pub(crate) timings: AppUiRenderTimings,
}

impl AppFrameContext {
    /// Возвращает единственный player snapshot текущего render frame-а.
    #[must_use]
    pub const fn player_snapshot(&self) -> &PlayerSnapshot {
        &self.player_snapshot
    }

    /// Возвращает render diagnostics, зафиксированные для текущего render frame-а.
    #[must_use]
    pub const fn render_diagnostics(&self) -> &RenderDiagnostics {
        &self.render_diagnostics
    }
}

/// Состояние приложения без владения playback pipeline.
pub struct AppState {
    /// Egui context — корневой объект egui для создания UI.
    pub egui_ctx: egui::Context,

    /// Egui_winit state — обработка ввода от winit.
    pub egui_winit_state: egui_winit::State,

    /// Desktop integration живёт отдельно от UI и говорит с player только через worker boundary.
    desktop_integration: Option<DesktopIntegration>,

    /// Playback worker владеет `PlayerSession` и media pipeline на отдельном thread.
    pub player_worker: PlayerWorker,

    /// Счётчик кадров shell-анимации.
    pub frame_index: u64,

    /// Время запуска приложения для расчёта elapsed time.
    pub start_time: std::time::Instant,

    /// Телеметрия — общие счётчики производительности.
    pub telemetry: Arc<Telemetry>,

    /// Read-only snapshot последнего committed config-а от authoritative settings runtime.
    committed_config_snapshot: CommittedConfigSnapshot,

    /// Read-only snapshot последнего capability report-а для app-owned selector-а.
    system_capabilities_snapshot: Option<SystemCapabilities>,

    /// Startup-ошибка shell-слоя, которую нужно показать без перевода player в Failed.
    pub startup_error: Option<String>,

    /// Pending-состояние shell-слоя для операций, которые ещё не дошли до player.
    pub startup_pending: Option<String>,

    /// Последний snapshot, уже доставленный UI; используется shell redraw pacing-ом.
    last_player_snapshot: PlayerSnapshot,

    /// Нужен один follow-up redraw после команды, которая уйдёт в worker асинхронно.
    pending_redraw_after_worker_command: bool,

    /// Последний кадр с уже полученными texture views для safe fallback на busy lock.
    cached_renderable_present_frame: Option<CachedRenderablePresentFrame>,

    /// App-owned main-video override во время scrub/seek visual pending state.
    main_visual_override_state: MainVisualOverrideState,

    /// Короткий inline статус timeline для scrub failures, owned только UI-слоем.
    timeline_inline_status: TimelineInlineStatusState,

    /// WGPU video materializer concrete backend-а; `player-core` его не видит.
    wgpu_frame_materializer: Option<Arc<dyn WgpuFrameTextureViewMaterializer>>,

    /// Класс активного video backend-а, чтобы не пересоздавать pipeline без нужды.
    current_video_backend_kind: Option<VideoBackendKind>,

    /// Отложенный запрос player-core на подбор backend-а под текущий стрим (`auto`).
    pending_video_backend_reselection: Option<VideoBackendSelectionRequest>,

    /// Requirement последнего активированного video-стрима, чтобы live-смена
    /// настроек (preference/decoder config) пересобирала pipeline с учётом того,
    /// какой именно кодек/профиль сейчас играет, а не выбирала backend вслепую.
    active_video_stream_requirement: Option<VideoDecodeRequirement>,

    /// Замороженный кадр прошлого backend-а на время живой смены backend/materializer:
    /// держится, пока worker не переключится и не выдаст первый кадр нового backend-а.
    backend_swap_frozen_frame: Option<CachedRenderablePresentFrame>,

    /// render generation на момент свапа; пока snapshot его не превысил, worker ещё не
    /// переключился, и кадры старого backend-а нельзя материализовать новым materializer-ом.
    backend_swap_from_generation: Option<u64>,

    /// Последний локальный файл, открытый shell-ом, для восстановления после suspend.
    current_local_file: Option<PathBuf>,

    /// Последний восстановимый media source intent для runtime source rebuild.
    active_media_source: Option<ActiveMediaSource>,

    /// Активный async dialog/prepare job для локального файла.
    local_file_open_job: Option<LocalFileOpenJob>,

    /// Transient pointer state timeline; player position здесь не хранится.
    timeline_ui_state: TimelineUiState,

    /// Кэш строк telemetry panel; живёт в UI-слое и не владеет playback/render state.
    telemetry_panel_cache: TelemetryPanelCache,

    /// Анимация выезда settings sidebar; runtime open-state остаётся целью перехода.
    sidebar_slide: SlideTransition,

    /// Момент последнего advance анимации sidebar для вычисления дельты времени кадра.
    sidebar_slide_last_tick: Option<Instant>,
}

/// Кап дельты времени кадра для анимации sidebar: после паузы/лага кадров
/// анимация продолжается плавно, а не прыгает к концу.
const MAX_SIDEBAR_SLIDE_FRAME_DT_SECONDS: f32 = 0.1;

impl AppState {
    /// Создаёт новое состояние приложения и запускает playback worker.
    #[instrument(skip(window, telemetry, committed_config_snapshot, startup_error))]
    pub fn new(
        window: &Window,
        telemetry: Arc<Telemetry>,
        committed_config_snapshot: CommittedConfigSnapshot,
        audio_output_device_controller: audio::AudioOutputDeviceController,
        startup_error: Option<String>,
    ) -> anyhow::Result<Self> {
        let egui_ctx = egui::Context::default();
        egui_ctx.set_theme(egui::Theme::Dark);

        let viewport_id = egui_ctx.viewport_id();
        let egui_winit_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window,
            Some(window.scale_factor() as f32),
            Some(winit::window::Theme::Dark),
            None,
        );
        let worker_config =
            PlayerWorkerConfig::from_app_config(committed_config_snapshot.as_config())
                .with_audio_decoder_factory(Arc::new(audio::ProductionAudioDecoderFactory))
                .with_audio_output_factory(Arc::new(audio::CpalAudioOutputFactory::new(
                    audio_output_device_controller,
                )))
                .with_audio_tempo_processor_factory(Arc::new(
                    audio_timestretch::TimestretchTempoProcessorFactory::default(),
                ));
        let player_worker = PlayerWorker::spawn(worker_config)?;
        let desktop_integration = match DesktopIntegration::spawn(player_worker.command_sender()) {
            Ok(desktop_integration) => Some(desktop_integration),
            Err(error) => {
                warn!(error = %error, "Не удалось запустить desktop integration");
                None
            }
        };
        if let Err(error) = player_worker.try_send_command(PlayerCommand::SetVolume(
            committed_config_snapshot.default_volume_for_new_media(),
        )) {
            warn!(error = %error, "Не удалось применить начальную громкость из config");
        }

        Ok(Self {
            egui_ctx,
            egui_winit_state,
            desktop_integration,
            player_worker,
            frame_index: 0,
            start_time: std::time::Instant::now(),
            telemetry,
            committed_config_snapshot,
            system_capabilities_snapshot: None,
            startup_error,
            startup_pending: None,
            last_player_snapshot: PlayerSnapshot::empty(),
            pending_redraw_after_worker_command: false,
            cached_renderable_present_frame: None,
            main_visual_override_state: MainVisualOverrideState::default(),
            timeline_inline_status: TimelineInlineStatusState::default(),
            wgpu_frame_materializer: None,
            current_video_backend_kind: None,
            pending_video_backend_reselection: None,
            active_video_stream_requirement: None,
            backend_swap_frozen_frame: None,
            backend_swap_from_generation: None,
            current_local_file: None,
            active_media_source: None,
            local_file_open_job: None,
            timeline_ui_state: TimelineUiState::default(),
            telemetry_panel_cache: TelemetryPanelCache::default(),
            sidebar_slide: SlideTransition::closed(),
            sidebar_slide_last_tick: None,
        })
    }

    /// Продвигает анимацию выезда settings sidebar к runtime open-state.
    ///
    /// Вызывается один раз за кадр до сборки settings UI model, чтобы видимость
    /// панели и video viewport считались по уже актуальной позиции анимации.
    pub(crate) fn advance_sidebar_slide(&mut self, settings_open: bool, now: Instant) {
        self.sidebar_slide.set_target_open(settings_open);

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
        self.sidebar_slide.advance(dt_seconds, duration_seconds);
    }

    /// `true`, пока анимация sidebar не достигла цели (нужен visual hold и repaint).
    #[must_use]
    pub(crate) fn sidebar_slide_is_animating(&self) -> bool {
        self.sidebar_slide.is_animating()
    }

    /// Обновляет read-only config snapshot из authoritative settings runtime.
    pub(crate) fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.committed_config_snapshot = snapshot;
        let live_scrub_settings = self.live_scrub_settings_snapshot();
        self.timeline_ui_state
            .defer_live_scrub_settings_change(live_scrub_settings);
    }

    /// Применяет player runtime settings через request/reply worker boundary.
    pub(crate) fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        self.player_worker.apply_runtime_settings(update)
    }

    /// Текущий decoder-thread config worker-а для app-owned pipeline rebuild.
    pub(crate) fn current_decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.player_worker.decoder_thread_config()
    }

    /// Применяет media/source policy через app-level owner.
    pub(crate) fn apply_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Applied
    }

    /// Переключает состояние воспроизведения через playback worker.
    pub fn toggle_playback(&mut self) {
        if let Err(error) = self
            .player_worker
            .try_send_command(PlayerCommand::TogglePlayback)
        {
            warn!(error = %error, "Не удалось переключить playback");
            return;
        }

        self.mark_pending_worker_redraw();
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

        AppFrameContext {
            player_snapshot,
            render_diagnostics,
        }
    }

    /// Показывает shell-level pending state, пока media ещё не передано в player.
    pub fn set_startup_pending(&mut self, message: String) {
        self.startup_error = None;
        self.startup_pending = Some(message);
        self.mark_pending_worker_redraw();
    }

    /// Показывает shell-level ошибку, которая возникла до открытия media в player.
    pub fn set_startup_error(&mut self, message: String) {
        self.startup_pending = None;
        self.startup_error = Some(message);
        self.mark_pending_worker_redraw();
    }

    /// Сбрасывает shell-level startup overlay после успешного открытия media.
    pub(super) fn clear_startup_status(&mut self) {
        self.startup_pending = None;
        self.startup_error = None;
        self.mark_pending_worker_redraw();
    }

    /// Обновляет кешированный read-only snapshot из `player-core` без внешней публикации.
    ///
    /// Это mutable boundary для worker snapshot: метод учитывает frame counters и обновляет
    /// `last_player_snapshot`, но не выполняет desktop integration side effects.
    #[must_use]
    pub(crate) fn refresh_player_snapshot(&mut self) -> PlayerSnapshot {
        let player_snapshot = self
            .player_worker
            .latest_snapshot(self.frame_counters_snapshot());
        self.last_player_snapshot = player_snapshot.clone();
        player_snapshot
    }

    /// Возвращает `true`, пока shell должен поддерживать непрерывные redraw-и.
    #[must_use]
    pub fn wants_continuous_redraw(&self) -> bool {
        self.last_player_snapshot
            .playback_state
            .is_playback_active()
            || self.last_player_snapshot.playback_state == PlaybackState::Opening
            || self.last_player_snapshot.playback_state == PlaybackState::Scrubbing
            || self.last_player_snapshot.timeline.scrubbing
    }

    /// Забирает одноразовый follow-up redraw после асинхронной worker command.
    pub fn take_pending_worker_redraw(&mut self) -> bool {
        std::mem::take(&mut self.pending_redraw_after_worker_command)
    }

    /// Помечает, что после текущего frame-а нужен ещё один redraw для worker response.
    pub(super) fn mark_pending_worker_redraw(&mut self) {
        self.pending_redraw_after_worker_command = true;
    }

    /// Явно публикует read-only snapshot в desktop integration boundary и забирает events.
    pub(crate) fn publish_desktop_snapshot(&self, player_snapshot: &PlayerSnapshot) {
        let Some(desktop_integration) = &self.desktop_integration else {
            return;
        };

        if let Err(error) = desktop_integration.publish_snapshot(player_snapshot) {
            warn!(error = %error, "Не удалось обновить desktop integration snapshot");
        }

        Self::log_desktop_integration_events(desktop_integration.drain_events());
    }

    /// Логирует события desktop integration без переноса MPRIS logic в UI.
    fn log_desktop_integration_events(events: Vec<DesktopIntegrationEvent>) {
        for event in events {
            match event {
                DesktopIntegrationEvent::BackendError { backend, error } => {
                    warn!(?backend, error = %error, "Desktop integration backend error");
                }
                other_event => {
                    debug!(?other_event, "Desktop integration event");
                }
            }
        }
    }
}
