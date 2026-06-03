/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use desktop_integration::{DesktopIntegration, DesktopIntegrationEvent};
use media_core::TrackKind;
use player_core::{
    FrameCounters, MediaOpenRequest, MediaSource, PlaybackState, PlayerCommand, PlayerError,
    PlayerErrorKind, PlayerEvent, PlayerPresentFrame, PlayerRenderError, PlayerSnapshot,
    PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent, PreparedMedia, SeekRequest,
};
use render_core::RenderDiagnostics;
use render_wgpu_video::{
    DmaBufWgpuFrameMaterializer, WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
    wrap_video_backend_for_wgpu_submission,
};
use rustiplayer_config::AppConfig;
use tracing::{debug, instrument, warn};
use video_vaapi::VaapiVideoBackendFactory;
use winit::window::Window;

use crate::local_file_open::{
    LocalFileOpenEvent, LocalFileOpenJob, LocalFileOpenResult, local_file_prepare_error_message,
    preparing_local_file_message,
};
use crate::local_media;
use crate::telemetry::Telemetry;
use crate::ui::animation::AnimationState;
use crate::ui::player_controls::{self, ControlAction};
use crate::ui::skin::{self, PlayerSkin};
use crate::ui::timeline::{self, TimelineAction, TimelineUiState};

/// Частота обновления тяжёлого текста telemetry panel.
///
/// Видео всё равно перерисовывает egui каждый кадр, но diagnostics-тексту достаточно
/// 4 Hz, чтобы не конкурировать с 60 fps video pacing.
const TELEMETRY_PANEL_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Начальная вместимость строк telemetry panel, чтобы refresh не делал лишних realloc.
const TELEMETRY_PANEL_ROW_CAPACITY: usize = 96;

/// Данные правой diagnostic panel, сгруппированные отдельно от UI output.
struct TelemetryPanelState<'panel> {
    /// Snapshot player-а на начало egui frame.
    player_snapshot: &'panel PlayerSnapshot,

    /// Shared telemetry counters.
    telemetry: &'panel Telemetry,

    /// Последняя renderer-neutral диагностика.
    render_diagnostics: &'panel RenderDiagnostics,

    /// Transient состояние timeline UI.
    timeline_ui_state: &'panel TimelineUiState,

    /// Имя активного backend-а для diagnostics.
    backend_name: &'panel str,

    /// Время запуска приложения.
    start_time: std::time::Instant,

    /// Оценка длительности video frame в миллисекундах.
    frame_duration_estimate_ms: f64,
}

/// Кэш уже отформатированных строк telemetry panel.
struct TelemetryPanelCache {
    /// Строки, которые egui будет раскладывать в текущих frame-ах.
    rows: Arc<[TelemetryPanelRow]>,

    /// Момент, после которого diagnostics нужно перечитать и переформатировать.
    next_refresh_at: Option<Instant>,
}

impl Default for TelemetryPanelCache {
    /// Создаёт пустой cache, который обновится при первом видимом кадре панели.
    fn default() -> Self {
        let rows: Arc<[TelemetryPanelRow]> = Vec::new().into();

        Self {
            rows,
            next_refresh_at: None,
        }
    }
}

impl TelemetryPanelCache {
    /// Возвращает строки для текущего кадра, обновляя тяжёлую diagnostics только по таймеру.
    fn rows_for_frame(
        &mut self,
        now: Instant,
        panel_state: TelemetryPanelState<'_>,
    ) -> Arc<[TelemetryPanelRow]> {
        if self.needs_refresh(now) {
            self.rows = AppState::build_telemetry_panel_rows(panel_state);
            self.next_refresh_at = Some(now + TELEMETRY_PANEL_REFRESH_INTERVAL);
        }

        Arc::clone(&self.rows)
    }

    /// Проверяет, пора ли перечитать counters/snapshot и собрать новый текст.
    fn needs_refresh(&self, now: Instant) -> bool {
        if self.rows.is_empty() {
            return true;
        }

        match self.next_refresh_at {
            Some(next_refresh_at) => now >= next_refresh_at,
            None => true,
        }
    }
}

/// Визуальный тон строки telemetry panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelemetryPanelRowTone {
    /// Обычная diagnostics-строка.
    Normal,

    /// Заголовок секции внутри virtualized списка.
    Heading,

    /// Успешное/здоровое состояние.
    Good,

    /// Пограничное состояние, которое стоит заметить.
    Warning,

    /// Ошибка или явно плохое состояние.
    Error,

    /// Пустая строка-разделитель.
    Spacer,
}

impl TelemetryPanelRowTone {
    /// Возвращает цвет текста, не меняя глобальный egui style.
    const fn color(self) -> egui::Color32 {
        match self {
            Self::Normal => egui::Color32::LIGHT_GRAY,
            Self::Heading => egui::Color32::LIGHT_BLUE,
            Self::Good => egui::Color32::GREEN,
            Self::Warning => egui::Color32::YELLOW,
            Self::Error => egui::Color32::RED,
            Self::Spacer => egui::Color32::TRANSPARENT,
        }
    }
}

/// Одна fixed-height строка virtualized telemetry panel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TelemetryPanelRow {
    /// Уже отформатированный текст, чтобы не собирать строки каждый video frame.
    text: String,

    /// Цветовой смысл строки.
    tone: TelemetryPanelRowTone,
}

impl TelemetryPanelRow {
    /// Создаёт обычную строку diagnostics.
    fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: TelemetryPanelRowTone::Normal,
        }
    }

    /// Создаёт строку-заголовок секции.
    fn heading(text: impl Into<String>) -> Self {
        Self {
            text: format!("[{}]", text.into()),
            tone: TelemetryPanelRowTone::Heading,
        }
    }

    /// Создаёт status-строку с явным цветовым тоном.
    fn status(text: impl Into<String>, tone: TelemetryPanelRowTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    /// Создаёт пустую строку без отдельного egui separator widget.
    fn spacer() -> Self {
        Self {
            text: String::new(),
            tone: TelemetryPanelRowTone::Spacer,
        }
    }

    /// Рендерит строку как single-line label, чтобы `show_rows` сохранял fixed row height.
    fn render(&self, ui: &mut egui::Ui) {
        let text = egui::RichText::new(self.text.as_str())
            .monospace()
            .color(self.tone.color());
        ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Truncate));
    }

    /// Возвращает текст строки для focused unit tests cache-а.
    #[cfg(test)]
    fn text(&self) -> &str {
        &self.text
    }
}

/// Diagnostic route пользовательского timeline intent-а на границе app-egui -> player-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineCommandRoute {
    /// Одиночный click ушёл в exact final seek без scrub generation.
    ClickSeek,

    /// Pointer drag release ушёл как exact final seek без scrub generation.
    DragSeek,
}

impl TimelineCommandRoute {
    /// Возвращает стабильное имя route-а для logs/diagnostics.
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClickSeek => "click-seek",
            Self::DragSeek => "drag-seek",
        }
    }
}

/// Конвертирует timeline action в player command и diagnostic route.
fn timeline_command_from_action(action: TimelineAction) -> (PlayerCommand, TimelineCommandRoute) {
    match action {
        TimelineAction::ClickSeek(position) => (
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::ClickSeek,
        ),
        TimelineAction::CommitDragSeek(position) => (
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::DragSeek,
        ),
    }
}

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

/// Явный результат получения frame lease-а для render boundary.
pub enum PresentFrameAcquisition {
    /// Worker ещё не публиковал zero-copy frame для текущей media session.
    NoFrameYet,

    /// Renderer повторяет последний безопасно удерживаемый lease.
    ReusedPreviousFrame(PlayerPresentFrame),

    /// Renderer получил новый lease с другим generation/texture handle.
    NewFrameAcquired(PlayerPresentFrame),

    /// Кандидат был отвергнут, потому что принадлежит старому render generation.
    StaleFrameRejected,
}

/// Кадр, для которого уже получены WGPU texture views и удерживается правильный render lease.
#[derive(Clone)]
pub struct RenderablePresentFrame {
    /// Lease удерживает backend texture resource до завершения render-side использования.
    pub present_frame: PlayerPresentFrame,

    /// WGPU texture views соответствуют `present_frame` и не используются без его lease-а.
    pub texture_views: WgpuFrameTextureViews,
}

impl RenderablePresentFrame {
    /// Собирает renderable frame из lease-а и WGPU texture views одного decoded кадра.
    #[must_use]
    pub fn new(present_frame: PlayerPresentFrame, texture_views: WgpuFrameTextureViews) -> Self {
        Self {
            present_frame,
            texture_views,
        }
    }
}

/// Cached renderable frame вместе с media source identity.
#[derive(Clone)]
struct CachedRenderablePresentFrame {
    /// Последний кадр, который точно прошёл WGPU texture view lookup.
    renderable_frame: RenderablePresentFrame,

    /// Source label защищает от reuse после открытия другого media.
    source_label: Option<String>,
}

/// Стабильная identity decoded кадра на renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentFrameIdentity {
    /// Поколение render resources, которому принадлежит texture handle.
    render_generation: u64,

    /// Opaque handle backend texture resource.
    resource_handle: video_core::FrameResourceHandle,

    /// Поколение decoded frame внутри текущего seek/decode lifecycle.
    decoded_generation: u64,

    /// Presentation timestamp decoded frame-а.
    pts: Duration,
}

impl PresentFrameIdentity {
    /// Создаёт identity из public lease fields без доступа к player pipeline.
    fn from_decoded_frame(render_generation: u64, frame: &video_core::DecodedFrame) -> Self {
        Self {
            render_generation,
            resource_handle: frame.resource_handle,
            decoded_generation: frame.generation,
            pts: frame.pts,
        }
    }
}

/// Причина явного освобождения cached present frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachedPresentFrameDiscardReason {
    /// Пользователь или shell начинает открывать другой media source.
    MediaOpenBoundary,

    /// Runtime с window/surface уничтожается целиком.
    RuntimeDrop,

    /// Player больше не держит текущий video frame для этой session.
    CurrentVideoFrameMissing,

    /// Cached frame относится к другому media source.
    SourceLabelChanged,

    /// Cached frame относится к старому render generation.
    RenderGenerationChanged,

    /// Swapchain/window lifecycle сделал cached texture небезопасной для удержания.
    SurfaceLifecycleBreak,

    /// Renderer/device path перешёл в fatal failure.
    RenderFailure,

    /// Worker сообщил render error вне текущего render call stack-а.
    WorkerRenderError,

    /// Player начал открытие media через event stream.
    PlayerMediaOpenRequested,

    /// Player завершил открытие media и source identity сменился на новую session.
    PlayerMediaOpened,

    /// Player остановил текущий media.
    PlayerStopped,

    /// Player перешёл в failed state.
    PlayerFailed,

    /// Player завершает session.
    PlayerShutdownRequested,

    /// Player сообщил fatal media/runtime error.
    PlayerFatalError,
}

/// Данные для pure-проверки, остаётся ли cached frame валидным для текущего player snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CachedPresentFrameValidationState {
    /// Есть ли у player-а текущий video frame в этой session.
    current_video_frame_present: bool,

    /// Совпадает ли media source cached frame-а с текущим source.
    source_matches: bool,

    /// Render generation cached frame-а.
    cached_generation: u64,

    /// Актуальное render generation из player snapshot-а.
    current_generation: u64,
}

/// Возвращает причину invalidation, если cached frame нельзя больше reuse-ить.
fn cached_present_frame_stale_reason(
    state: CachedPresentFrameValidationState,
) -> Option<CachedPresentFrameDiscardReason> {
    if !state.current_video_frame_present {
        return Some(CachedPresentFrameDiscardReason::CurrentVideoFrameMissing);
    }

    if !state.source_matches {
        return Some(CachedPresentFrameDiscardReason::SourceLabelChanged);
    }

    if state.cached_generation != state.current_generation {
        return Some(CachedPresentFrameDiscardReason::RenderGenerationChanged);
    }

    None
}

/// Мапит player event stream в cache lifecycle invalidation.
fn cached_present_frame_discard_reason_for_player_event(
    player_event: &PlayerEvent,
) -> Option<CachedPresentFrameDiscardReason> {
    match player_event {
        PlayerEvent::MediaOpenRequested(_) => {
            Some(CachedPresentFrameDiscardReason::PlayerMediaOpenRequested)
        }
        PlayerEvent::MediaOpened(_) => Some(CachedPresentFrameDiscardReason::PlayerMediaOpened),
        PlayerEvent::PlaybackStateChanged(PlaybackState::Stopped) => {
            Some(CachedPresentFrameDiscardReason::PlayerStopped)
        }
        PlayerEvent::PlaybackStateChanged(PlaybackState::Failed) => {
            Some(CachedPresentFrameDiscardReason::PlayerFailed)
        }
        PlayerEvent::ShutdownRequested => {
            Some(CachedPresentFrameDiscardReason::PlayerShutdownRequested)
        }
        PlayerEvent::FatalError(_) => Some(CachedPresentFrameDiscardReason::PlayerFatalError),
        PlayerEvent::PlaybackStateChanged(_)
        | PlayerEvent::PositionChanged(_)
        | PlayerEvent::SeekRequested(_)
        | PlayerEvent::SeekTargetFramePresented(_)
        | PlayerEvent::SeekCommitted(_)
        | PlayerEvent::AudioResumedAfterSeek(_)
        | PlayerEvent::VideoFrameReady(_)
        | PlayerEvent::BufferingStateChanged(_)
        | PlayerEvent::CapabilityScanCompleted(_)
        | PlayerEvent::VideoTrackSelected(_)
        | PlayerEvent::AudioTrackSelected(_)
        | PlayerEvent::SubtitleTrackSelected(_)
        | PlayerEvent::QualitySelectionChanged(_)
        | PlayerEvent::ConfigReloadRequested
        | PlayerEvent::RecoverableError(_) => None,
    }
}

/// Минимальная state-модель для проверки safe previous-frame reuse без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextureBusyFallbackReuseState {
    /// Render generation cached frame-а.
    cached_generation: u64,

    /// Актуальное render generation из player snapshot-а.
    current_generation: u64,

    /// Совпадает ли media source cached frame-а с текущим source.
    source_matches: bool,

    /// Есть ли у player-а текущий video frame для этой media session.
    has_current_video_frame: bool,

    /// Был ли cached frame уже помечен stale при публикации lease-а.
    cached_frame_is_stale: bool,

    /// Помечает ли session текущую картинку stale относительно seek/scrub состояния.
    ///
    /// Pending scrub target сам по себе не выставляет этот флаг, пока cached frame остаётся
    /// валидным относительно source и render generation.
    timeline_marks_frame_stale: bool,
}

/// Причина, по которой Busy fallback не имеет права повторить cached frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureBusyFallbackRejectReason {
    /// Cached frame относится к старому render generation.
    RenderGenerationChanged,

    /// Cached frame был получен для другого media source.
    SourceLabelChanged,

    /// Player snapshot больше не содержит текущий video frame.
    CurrentVideoFrameMissing,

    /// Cached lease уже был помечен stale на render boundary.
    CachedFrameStale,

    /// Timeline сейчас считает текущий кадр stale относительно seek/scrub.
    TimelineFrameStale,
}

/// Возвращает причину отказа Busy fallback-а или `None`, если reuse безопасен.
fn texture_busy_fallback_reject_reason(
    state: TextureBusyFallbackReuseState,
) -> Option<TextureBusyFallbackRejectReason> {
    if state.cached_generation != state.current_generation {
        return Some(TextureBusyFallbackRejectReason::RenderGenerationChanged);
    }
    if !state.source_matches {
        return Some(TextureBusyFallbackRejectReason::SourceLabelChanged);
    }
    if !state.has_current_video_frame {
        return Some(TextureBusyFallbackRejectReason::CurrentVideoFrameMissing);
    }
    if state.cached_frame_is_stale {
        return Some(TextureBusyFallbackRejectReason::CachedFrameStale);
    }
    if state.timeline_marks_frame_stale {
        return Some(TextureBusyFallbackRejectReason::TimelineFrameStale);
    }

    None
}

/// Решает, можно ли использовать previous renderable frame при busy texture lock-е.
#[cfg(test)]
fn texture_busy_fallback_can_reuse_previous_frame(state: TextureBusyFallbackReuseState) -> bool {
    texture_busy_fallback_reject_reason(state).is_none()
}

impl PresentFrameAcquisition {
    /// Возвращает frame lease, если acquisition state разрешает rendering video.
    #[must_use]
    pub fn into_present_frame(self) -> Option<PlayerPresentFrame> {
        match self {
            Self::ReusedPreviousFrame(present_frame) | Self::NewFrameAcquired(present_frame) => {
                Some(present_frame)
            }
            Self::NoFrameYet | Self::StaleFrameRejected => None,
        }
    }

    /// Возвращает `true`, если render tick повторно использует предыдущий frame.
    #[must_use]
    pub const fn reused_previous_frame(&self) -> bool {
        matches!(self, Self::ReusedPreviousFrame(_))
    }

    /// Стабильное имя state для trace diagnostics.
    #[must_use]
    pub const fn metric_name(&self) -> &'static str {
        match self {
            Self::NoFrameYet => "no_frame_yet",
            Self::ReusedPreviousFrame(_) => "reused_previous_frame",
            Self::NewFrameAcquired(_) => "new_frame_acquired",
            Self::StaleFrameRejected => "stale_frame_rejected",
        }
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

    /// Валидированная пользовательская конфигурация.
    pub app_config: AppConfig,

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

    /// WGPU video materializer concrete backend-а; `player-core` его не видит.
    wgpu_frame_materializer: Option<Arc<dyn WgpuFrameTextureViewMaterializer>>,

    /// Последний локальный файл, открытый shell-ом, для восстановления после suspend.
    current_local_file: Option<PathBuf>,

    /// Активный async dialog/prepare job для локального файла.
    local_file_open_job: Option<LocalFileOpenJob>,

    /// Transient pointer state timeline; player position здесь не хранится.
    timeline_ui_state: TimelineUiState,

    /// Кэш строк telemetry panel; живёт в UI-слое и не владеет playback/render state.
    telemetry_panel_cache: TelemetryPanelCache,

    /// Версия приложения для отображения в UI.
    pub app_version: &'static str,
}

impl AppState {
    /// Создаёт новое состояние приложения и запускает playback worker.
    #[instrument(skip(window, telemetry, app_config, startup_error))]
    pub fn new(
        window: &Window,
        telemetry: Arc<Telemetry>,
        app_config: AppConfig,
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
        let worker_config = PlayerWorkerConfig::from_app_config(&app_config)
            .with_audio_decoder_factory(Arc::new(audio::ProductionAudioDecoderFactory))
            .with_audio_output_factory(Arc::new(audio::CpalAudioOutputFactory));
        let player_worker = PlayerWorker::spawn(worker_config)?;
        let desktop_integration = match DesktopIntegration::spawn(player_worker.command_sender()) {
            Ok(desktop_integration) => Some(desktop_integration),
            Err(error) => {
                warn!(error = %error, "Не удалось запустить desktop integration");
                None
            }
        };
        if let Err(error) =
            player_worker.try_send_command(PlayerCommand::SetVolume(app_config.audio.volume as f32))
        {
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
            app_config,
            startup_error,
            startup_pending: None,
            last_player_snapshot: PlayerSnapshot::empty(),
            pending_redraw_after_worker_command: false,
            cached_renderable_present_frame: None,
            wgpu_frame_materializer: None,
            current_local_file: None,
            local_file_open_job: None,
            timeline_ui_state: TimelineUiState::default(),
            telemetry_panel_cache: TelemetryPanelCache::default(),
            app_version: env!("CARGO_PKG_VERSION"),
        })
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
    fn clear_startup_status(&mut self) {
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
            || self.last_player_snapshot.timeline.scrubbing
    }

    /// Забирает одноразовый follow-up redraw после асинхронной worker command.
    pub fn take_pending_worker_redraw(&mut self) -> bool {
        std::mem::take(&mut self.pending_redraw_after_worker_command)
    }

    /// Помечает, что после текущего frame-а нужен ещё один redraw для worker response.
    fn mark_pending_worker_redraw(&mut self) {
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

    /// Загружает локальный файл через playback worker.
    pub fn load_file(&mut self, path: &Path) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = Some(path.to_path_buf());

        match local_media::prepare_local_file(path, &self.app_config.player.demux) {
            Ok(prepared_media) => {
                if let Err(error) = self
                    .player_worker
                    .load_prepared_media(prepared_media, autoplay)
                {
                    warn!(error = %error, "Не удалось отправить подготовленный файл в worker");
                    return;
                }
            }
            Err(error) => {
                warn!(error = %error, "Не удалось открыть файл");
                let open_request =
                    MediaOpenRequest::new(MediaSource::LocalFile(path.to_path_buf()), autoplay);
                let player_error =
                    PlayerError::new(PlayerErrorKind::DemuxError, format!("Ошибка: {error}"));
                if let Err(send_error) = self
                    .player_worker
                    .fail_media_open(open_request, player_error)
                {
                    warn!(error = %send_error, "Не удалось отправить ошибку открытия файла в worker");
                    return;
                }
            }
        }

        self.mark_pending_worker_redraw();
    }

    /// Доставляет уже подготовленный локальный media в worker после async UI opening-а.
    fn load_prepared_local_file(&mut self, path: PathBuf, prepared_media: PreparedMedia) {
        let autoplay = !self.app_config.player.start_paused;

        if let Err(error) = self
            .player_worker
            .load_prepared_media(prepared_media, autoplay)
        {
            warn!(error = %error, path = %path.display(), "Не удалось отправить подготовленный файл в worker");
            self.set_startup_error(format!(
                "Ошибка открытия media-файла {}: worker недоступен: {error}",
                path.display()
            ));
            return;
        }

        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = Some(path);
        self.mark_pending_worker_redraw();
    }

    /// Загружает YouTube demuxer без долговременного database/cache слоя.
    pub fn load_youtube_demuxer(
        &mut self,
        label: String,
        demuxer: Box<dyn symphonia_demux::Demuxer + Send>,
    ) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = None;
        if let Err(error) = self.player_worker.load_demuxer(label, demuxer, autoplay) {
            warn!(error = %error, "Не удалось отправить YouTube demuxer в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Загружает уже подготовленный внешний media source через PreparedMedia boundary.
    pub fn load_prepared_external_media(&mut self, label: String, prepared_media: PreparedMedia) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = None;

        if let Err(error) = self
            .player_worker
            .load_prepared_media(prepared_media, autoplay)
        {
            warn!(error = %error, label = %label, "Не удалось отправить внешний media source в worker");
            self.set_startup_error(format!(
                "WorkerUnavailable: direct media worker недоступен для {label}: {error}"
            ));
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Инициализирует video pipeline и сохраняет WGPU materializer в shell layer-е.
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let decoder_thread_config = self.player_worker.decoder_thread_config();
        let backend_factory =
            VaapiVideoBackendFactory::new_with_decoder_config(decoder_thread_config);

        let started_backend = match backend_factory.start_for_composition() {
            Ok(started_backend) => started_backend,
            Err(error) => {
                warn!(error = %error, "Video backend unavailable, no hardware decode");
                return;
            }
        };
        let (player_backend, frame_resource_provider) =
            wrap_video_backend_for_wgpu_submission(started_backend, queue);
        self.wgpu_frame_materializer = Some(Arc::new(DmaBufWgpuFrameMaterializer::new(
            instance,
            adapter,
            device,
            frame_resource_provider,
        )));

        if let Err(error) = self.player_worker.set_video_backend(player_backend) {
            self.wgpu_frame_materializer = None;
            warn!(error = %error, "Не удалось отправить video backend в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Возвращает WGPU materializer текущего concrete video backend-а.
    pub(crate) fn wgpu_frame_materializer(
        &self,
    ) -> Option<Arc<dyn WgpuFrameTextureViewMaterializer>> {
        self.wgpu_frame_materializer.clone()
    }

    /// Передаёт capability report из shell/backend layer в playback worker.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        if let Err(error) = self.player_worker.set_system_capabilities(capabilities) {
            warn!(error = %error, "Не удалось отправить capability report в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Пытается получить текущий video frame для renderer-а.
    #[must_use]
    pub fn acquire_present_frame_for_render(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> PresentFrameAcquisition {
        let rejected_stale_cached_frame = self.drop_stale_cached_present_frame(player_snapshot);

        if let Some(mut present_frame) = self.player_worker.try_acquire_present_frame() {
            if present_frame.render_generation != player_snapshot.render_generation {
                self.clear_cached_present_frame(
                    CachedPresentFrameDiscardReason::RenderGenerationChanged,
                );
                return PresentFrameAcquisition::StaleFrameRejected;
            }

            present_frame.stale = present_frame.stale || player_snapshot.timeline.stale_frame;
            let cached_frame_identity =
                self.cached_renderable_present_frame
                    .as_ref()
                    .map(|cached_frame| {
                        Self::present_frame_identity(&cached_frame.renderable_frame.present_frame)
                    });
            let acquired_frame_identity = Self::present_frame_identity(&present_frame);

            if cached_frame_identity == Some(acquired_frame_identity) {
                return self
                    .cached_renderable_present_frame
                    .as_ref()
                    .map(|cached_frame| cached_frame.renderable_frame.present_frame.clone())
                    .map(|mut cached_present_frame| {
                        cached_present_frame.stale = present_frame.stale;
                        PresentFrameAcquisition::ReusedPreviousFrame(cached_present_frame)
                    })
                    .unwrap_or(PresentFrameAcquisition::NoFrameYet);
            }

            return PresentFrameAcquisition::NewFrameAcquired(present_frame);
        }

        if let Some(mut cached_present_frame) = self
            .cached_renderable_present_frame
            .as_ref()
            .map(|cached_frame| cached_frame.renderable_frame.present_frame.clone())
        {
            cached_present_frame.stale =
                cached_present_frame.stale || player_snapshot.timeline.stale_frame;
            return PresentFrameAcquisition::ReusedPreviousFrame(cached_present_frame);
        }

        if rejected_stale_cached_frame {
            PresentFrameAcquisition::StaleFrameRejected
        } else {
            PresentFrameAcquisition::NoFrameYet
        }
    }

    /// Сбрасывает cached frame, когда он уже не принадлежит текущему media/render поколению.
    fn drop_stale_cached_present_frame(&mut self, player_snapshot: &PlayerSnapshot) -> bool {
        let Some(cached_renderable_frame) = &self.cached_renderable_present_frame else {
            return false;
        };

        let validation_state = CachedPresentFrameValidationState {
            current_video_frame_present: player_snapshot.current_video_frame.is_some(),
            source_matches: cached_renderable_frame.source_label.as_deref()
                == player_snapshot.source_label.as_deref(),
            cached_generation: cached_renderable_frame
                .renderable_frame
                .present_frame
                .render_generation,
            current_generation: player_snapshot.render_generation,
        };
        let Some(reason) = cached_present_frame_stale_reason(validation_state) else {
            return false;
        };
        let rejected_generation =
            reason == CachedPresentFrameDiscardReason::RenderGenerationChanged;

        self.clear_cached_present_frame(reason);
        rejected_generation
    }

    /// Возвращает identity frame-а, достаточную для отличия нового lease-а от reuse.
    fn present_frame_identity(present_frame: &PlayerPresentFrame) -> PresentFrameIdentity {
        PresentFrameIdentity::from_decoded_frame(
            present_frame.render_generation,
            &present_frame.frame,
        )
    }

    /// Освобождает cached present frame и отправляет drop-ack worker-у через lease guard.
    fn clear_cached_present_frame(&mut self, reason: CachedPresentFrameDiscardReason) {
        if self.cached_renderable_present_frame.is_some() {
            debug!(?reason, "Clearing cached present frame");
        }
        self.cached_renderable_present_frame = None;
    }

    /// Освобождает cached frame перед уничтожением app/window runtime.
    pub fn clear_cached_present_frame_for_runtime_drop(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::RuntimeDrop);
    }

    /// Освобождает cached frame после swapchain/surface lifecycle break-а.
    pub fn clear_cached_present_frame_after_surface_lifecycle_break(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::SurfaceLifecycleBreak);
    }

    /// Освобождает cached frame после renderer/device failure.
    pub fn clear_cached_present_frame_after_render_failure(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::RenderFailure);
    }

    /// Освобождает cached frame после worker-side render error event-а.
    pub fn clear_cached_present_frame_after_worker_render_error(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::WorkerRenderError);
    }

    /// Синхронизирует cache lifecycle с событиями player state machine.
    pub fn handle_cached_present_frame_player_event(&mut self, player_event: &PlayerEvent) {
        let Some(reason) = cached_present_frame_discard_reason_for_player_event(player_event)
        else {
            return;
        };

        self.clear_cached_present_frame(reason);
    }

    /// Запоминает последний frame, который реально получил texture views.
    pub fn remember_renderable_present_frame(
        &mut self,
        renderable_frame: RenderablePresentFrame,
        player_snapshot: &PlayerSnapshot,
    ) {
        self.cached_renderable_present_frame = Some(CachedRenderablePresentFrame {
            renderable_frame,
            source_label: player_snapshot.source_label.clone(),
        });
    }

    /// Возвращает previous renderable frame для busy fallback, если lifecycle всё ещё валиден.
    #[must_use]
    pub fn reusable_renderable_frame_for_texture_busy(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<RenderablePresentFrame> {
        self.drop_stale_cached_present_frame(player_snapshot);

        let cached_renderable_frame = self.cached_renderable_present_frame.as_ref()?;
        let reuse_state = TextureBusyFallbackReuseState {
            cached_generation: cached_renderable_frame
                .renderable_frame
                .present_frame
                .render_generation,
            current_generation: player_snapshot.render_generation,
            source_matches: cached_renderable_frame.source_label.as_deref()
                == player_snapshot.source_label.as_deref(),
            has_current_video_frame: player_snapshot.current_video_frame.is_some(),
            cached_frame_is_stale: cached_renderable_frame.renderable_frame.present_frame.stale,
            timeline_marks_frame_stale: player_snapshot.timeline.stale_frame,
        };

        if let Some(reject_reason) = texture_busy_fallback_reject_reason(reuse_state) {
            debug!(
                ?reject_reason,
                "Texture view Busy fallback rejected cached renderable frame"
            );
            return None;
        }

        Some(cached_renderable_frame.renderable_frame.clone())
    }

    /// Передаёт typed render bridge error в worker-owned player session.
    pub fn report_render_error(&mut self, error: PlayerRenderError) {
        if let Err(send_error) = self.player_worker.report_render_error(error) {
            warn!(error = %send_error, "Не удалось отправить typed render error в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Передаёт renderer submit/present timing в player diagnostics без render-side business logic.
    pub fn report_gpu_submit_present_latency(&self, submit_present_elapsed: Duration) {
        self.player_worker
            .report_gpu_submit_present_latency(submit_present_elapsed);
    }

    /// Передаёт player diagnostics событие reuse previous frame-а из-за busy resource lock-а.
    pub fn report_render_resource_previous_frame_reuse(&self) {
        self.player_worker
            .report_render_resource_previous_frame_reuse();
    }

    /// Забирает worker events для shell telemetry.
    #[must_use]
    pub fn drain_worker_events(&mut self) -> Vec<PlayerWorkerEvent> {
        self.player_worker.drain_events()
    }

    /// Возвращает последний локальный файл, открытый shell-ом.
    #[must_use]
    pub fn current_local_file(&self) -> Option<&Path> {
        self.current_local_file.as_deref()
    }

    /// Возвращает `true`, пока shell ждёт file dialog или подготовку локального media.
    #[must_use]
    pub fn has_pending_local_file_open(&self) -> bool {
        self.local_file_open_job.is_some()
    }

    /// Неблокирующе забирает события async открытия локального файла.
    pub fn poll_local_file_open_job(&mut self) {
        let mut finished_result = None;

        while let Some(event) = self
            .local_file_open_job
            .as_mut()
            .and_then(LocalFileOpenJob::try_take_event)
        {
            match event {
                LocalFileOpenEvent::Preparing { path } => {
                    self.set_startup_pending(preparing_local_file_message(&path));
                }
                LocalFileOpenEvent::Finished(result) => {
                    finished_result = Some(result);
                    break;
                }
            }
        }

        let Some(mut result) = finished_result else {
            return;
        };

        if let Some(join_error) = self
            .local_file_open_job
            .as_mut()
            .and_then(LocalFileOpenJob::join_after_finished)
        {
            result = LocalFileOpenResult::JobFailed { error: join_error };
        }
        self.local_file_open_job = None;

        self.apply_local_file_open_result(result);
    }

    /// Применяет финальный результат local open job-а к shell и worker boundary.
    fn apply_local_file_open_result(&mut self, result: LocalFileOpenResult) {
        match result {
            LocalFileOpenResult::Cancelled => {
                self.startup_pending = None;
                self.mark_pending_worker_redraw();
            }
            LocalFileOpenResult::Prepared {
                path,
                prepared_media,
            } => {
                self.load_prepared_local_file(path, prepared_media);
            }
            LocalFileOpenResult::PrepareFailed { path, error } => {
                warn!(path = %path.display(), error = %error, "Не удалось подготовить локальный файл");
                self.set_startup_error(local_file_prepare_error_message(&path, &error));
            }
            LocalFileOpenResult::JobFailed { error } => {
                warn!(error = %error, "Local file open job завершился ошибкой");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }

    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure отправляет worker-у.
    #[instrument(skip(self, window, frame_context))]
    pub fn render_ui(
        &mut self,
        window: &Window,
        egui_input: egui::RawInput,
        frame_context: &AppFrameContext,
    ) -> RenderedAppUi {
        let render_ui_started_at = Instant::now();

        let pre_ui_setup_started_at = Instant::now();
        let player_snapshot = frame_context.player_snapshot();
        let is_playing = player_snapshot.playback_state == PlaybackState::Playing;

        if is_playing {
            self.next_frame();
        }

        let backend_name = player_snapshot
            .active_backend
            .name
            .clone()
            .unwrap_or_else(|| "Synthetic (test)".to_string());
        let app_version = self.app_version;
        let telemetry = Arc::clone(&self.telemetry);
        let start_time = self.start_time;
        let frame_duration_estimate_ms =
            player_snapshot.video_frame_duration_estimate.as_secs_f64() * 1000.0;
        let player_error_message = player_snapshot
            .last_error
            .as_ref()
            .map(std::string::ToString::to_string);
        let error_message = player_error_message
            .as_deref()
            .or(self.startup_error.as_deref());
        let pending_message = if error_message.is_none() {
            self.startup_pending.as_deref()
        } else {
            None
        };
        let render_diagnostics = frame_context.render_diagnostics();
        let selected_skin = skin::skin_from_config(&self.app_config.ui.skin).unwrap_or_else(|| {
            warn!(
                skin = %self.app_config.ui.skin,
                "Config validation должна была отклонить неизвестный UI skin; используем minimal"
            );
            skin::MinimalSkin
        });
        let animation_state = AnimationState::from_timeline(&player_snapshot.timeline);
        let show_telemetry = self.app_config.ui.show_telemetry;
        let mut control_actions = Vec::new();
        let mut timeline_ui_state = std::mem::take(&mut self.timeline_ui_state);
        let pre_ui_setup_elapsed = pre_ui_setup_started_at.elapsed();
        let mut telemetry_panel_cache_elapsed = Duration::ZERO;
        let telemetry_panel_rows = if show_telemetry {
            let telemetry_panel_cache_started_at = Instant::now();
            let panel_rows = self.telemetry_panel_cache.rows_for_frame(
                Instant::now(),
                TelemetryPanelState {
                    player_snapshot,
                    telemetry: &telemetry,
                    render_diagnostics,
                    timeline_ui_state: &timeline_ui_state,
                    backend_name: &backend_name,
                    start_time,
                    frame_duration_estimate_ms,
                },
            );
            telemetry_panel_cache_elapsed = telemetry_panel_cache_started_at.elapsed();
            Some(panel_rows)
        } else {
            None
        };

        let mut top_bar_elapsed = Duration::ZERO;
        let mut bottom_controls_elapsed = Duration::ZERO;
        let mut telemetry_panel_elapsed = Duration::ZERO;
        let mut center_overlay_elapsed = Duration::ZERO;

        let egui_run_started_at = Instant::now();
        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            let stage_started_at = Instant::now();
            player_controls::render_top_bar(ui, app_version, &selected_skin);
            top_bar_elapsed = stage_started_at.elapsed();

            let stage_started_at = Instant::now();
            control_actions = player_controls::render_bottom_controls(
                ui,
                player_snapshot,
                &mut timeline_ui_state,
                &selected_skin,
            );
            bottom_controls_elapsed = stage_started_at.elapsed();

            if let Some(telemetry_panel_rows) = telemetry_panel_rows.as_ref() {
                let stage_started_at = Instant::now();
                Self::render_telemetry_panel(ui, telemetry_panel_rows);
                telemetry_panel_elapsed =
                    telemetry_panel_cache_elapsed + stage_started_at.elapsed();
            }

            let stage_started_at = Instant::now();
            Self::render_center_overlay(
                ui,
                is_playing,
                error_message,
                pending_message,
                &selected_skin,
                animation_state,
            );
            center_overlay_elapsed = stage_started_at.elapsed();
        });
        let egui_run_elapsed = egui_run_started_at.elapsed();

        let post_ui_actions_started_at = Instant::now();
        self.timeline_ui_state = timeline_ui_state;
        self.handle_control_actions(window, control_actions);
        let post_ui_actions_elapsed = post_ui_actions_started_at.elapsed();

        RenderedAppUi {
            full_output,
            timings: AppUiRenderTimings {
                total: render_ui_started_at.elapsed(),
                pre_ui_setup: pre_ui_setup_elapsed,
                egui_run: egui_run_elapsed,
                top_bar: top_bar_elapsed,
                bottom_controls: bottom_controls_elapsed,
                telemetry_panel: telemetry_panel_elapsed,
                center_overlay: center_overlay_elapsed,
                post_ui_actions: post_ui_actions_elapsed,
                telemetry_panel_visible: show_telemetry,
            },
        }
    }

    /// Применяет действия controls после завершения egui pass.
    fn handle_control_actions(&mut self, window: &Window, actions: Vec<ControlAction>) {
        for action in actions {
            match action {
                ControlAction::TogglePlayback => self.toggle_playback(),
                ControlAction::OpenFile => self.open_file(window),
                ControlAction::SetVolume(requested_volume) => {
                    if let Err(error) = self
                        .player_worker
                        .try_send_command(PlayerCommand::SetVolume(requested_volume))
                    {
                        warn!(error = %error, "Некорректное значение громкости из UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleFullscreen => Self::toggle_fullscreen(window),
                ControlAction::Timeline(timeline_action) => {
                    self.send_timeline_action(timeline_action);
                }
            }
        }
    }

    /// Конвертирует pointer timeline action в typed player command.
    ///
    /// Здесь находится единственный app-egui route, который переводит timeline intent
    /// в worker command. Click и drag release отправляются как `PlayerCommand::Seek`,
    /// чтобы UI не зависел от interactive scrub lifecycle внутри player-core.
    fn send_timeline_action(&mut self, action: TimelineAction) {
        let (command, route) = timeline_command_from_action(action);
        let command_for_diagnostics = command.clone();

        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось отправить timeline command");
            return;
        }

        debug!(
            action = ?action,
            command = ?command_for_diagnostics,
            route = route.as_str(),
            "Timeline action отправлен в player worker"
        );
        self.mark_pending_worker_redraw();
    }

    /// Переключает fullscreen состояние окна.
    fn toggle_fullscreen(window: &Window) {
        let is_fullscreen = window.fullscreen().is_some();
        if is_fullscreen {
            window.set_fullscreen(None);
            return;
        }

        if let Some(monitor) = window.current_monitor() {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(Some(monitor))));
        }
    }

    /// Обрабатывает горячие клавиши shell и отправляет команды в playback worker.
    pub fn handle_hotkeys(
        &mut self,
        window: &Window,
        key: winit::keyboard::KeyCode,
        egui_wants_input: bool,
    ) -> bool {
        if egui_wants_input {
            return false;
        }

        match key {
            winit::keyboard::KeyCode::Space => {
                self.toggle_playback();
                true
            }
            winit::keyboard::KeyCode::KeyF => {
                Self::toggle_fullscreen(window);
                true
            }
            winit::keyboard::KeyCode::KeyM => {
                let player_snapshot = self.refresh_player_snapshot();
                self.publish_desktop_snapshot(&player_snapshot);
                let current_volume = player_snapshot.volume;
                let next_volume = if current_volume > 0.0 {
                    0.0
                } else {
                    self.app_config.audio.volume as f32
                };
                if let Err(error) = self
                    .player_worker
                    .try_send_command(PlayerCommand::SetVolume(next_volume))
                {
                    warn!(error = %error, "Не удалось переключить mute");
                } else {
                    self.mark_pending_worker_redraw();
                }
                true
            }
            _ => false,
        }
    }

    /// Открывает локальный media-файл через file dialog.
    pub fn open_file(&mut self, window: &Window) {
        if self.has_pending_local_file_open() {
            debug!("Local file open job уже активен, повторный dialog не запускаем");
            return;
        }

        match LocalFileOpenJob::spawn(window, self.app_config.player.demux) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending("Выбор media-файла...".to_string());
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить local file open job");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }

    /// Собирает frame counters из текущей телеметрии.
    fn frame_counters_snapshot(&self) -> FrameCounters {
        FrameCounters {
            presented: self.telemetry.video_frames_presented(),
            dropped: self.telemetry.video_frames_dropped(),
            repeated: self.telemetry.video_frames_repeated(),
        }
    }

    /// Рендерит правую диагностическую панель из заранее отформатированных строк.
    fn render_telemetry_panel(ui: &mut egui::Ui, panel_rows: &[TelemetryPanelRow]) {
        let telemetry_frame =
            egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160));
        egui::Panel::right("telemetry")
            .resizable(true)
            .default_size(280.0)
            .size_range(220.0..=520.0)
            .frame(telemetry_frame)
            .show_inside(ui, |ui| {
                ui.heading("Telemetry");
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("telemetry_scroll")
                    .auto_shrink([false, false])
                    .show_rows(
                        ui,
                        Self::telemetry_panel_row_height(ui),
                        panel_rows.len(),
                        |ui, visible_rows| {
                            for row_index in visible_rows {
                                if let Some(row) = panel_rows.get(row_index) {
                                    row.render(ui);
                                }
                            }
                        },
                    );
            });
    }

    /// Возвращает fixed-height строки для `ScrollArea::show_rows`.
    fn telemetry_panel_row_height(ui: &egui::Ui) -> f32 {
        ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y
    }

    /// Собирает cached модель telemetry panel без доступа к renderer/player internals.
    fn build_telemetry_panel_rows(
        panel_state: TelemetryPanelState<'_>,
    ) -> Arc<[TelemetryPanelRow]> {
        let mut panel_rows = Vec::with_capacity(TELEMETRY_PANEL_ROW_CAPACITY);

        Self::append_telemetry_summary_rows(&mut panel_rows, &panel_state);
        Self::append_media_info_rows(&mut panel_rows, &panel_state);

        panel_rows.into()
    }

    /// Добавляет верхнюю summary-секцию telemetry panel.
    fn append_telemetry_summary_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let telemetry = panel_state.telemetry;
        let fps = telemetry.current_fps();
        let frame_time = telemetry.last_frame_time_ms();
        let frames_presented_to_surface = telemetry.frames_presented_to_surface();
        let surface_dropped_frames = telemetry.surface_dropped_frames();
        let surface_drop_rate = telemetry.surface_drop_rate_percent();
        let frame_pacing_tone = if fps >= 55 {
            TelemetryPanelRowTone::Good
        } else if fps >= 30 {
            TelemetryPanelRowTone::Warning
        } else {
            TelemetryPanelRowTone::Error
        };
        let frame_pacing_text = if fps >= 55 {
            "Frame pacing: OK"
        } else if fps >= 30 {
            "Frame pacing: warning"
        } else {
            "Frame pacing: bad"
        };

        panel_rows.push(TelemetryPanelRow::normal(format!("FPS: {fps}")));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Frame time: {:.2} ms",
            frame_time as f64
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Swapchain"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "frames_presented_to_surface: {frames_presented_to_surface}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "surface_dropped_frames: {surface_dropped_frames}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "surface_drop_rate: {surface_drop_rate:.2}%"
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Smoothness"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback visible drops: {}",
            telemetry.playback_visible_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "repeated_frames: {}",
            telemetry.repeated_frames()
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Seek"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Seek discard, expected: {}",
            telemetry.seek_discarded_frames()
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::status(
            frame_pacing_text,
            frame_pacing_tone,
        ));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Backend: {}",
            panel_state.backend_name
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Elapsed: {:.1}s",
            panel_state.start_time.elapsed().as_secs_f64()
        )));
    }

    /// Добавляет media/player diagnostics в cached строки telemetry panel.
    fn append_media_info_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let player_snapshot = panel_state.player_snapshot;
        let telemetry = panel_state.telemetry;

        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Media Info"));

        if player_snapshot.source_label.is_none() {
            panel_rows.push(TelemetryPanelRow::normal("No file loaded"));
            return;
        }

        for track in &player_snapshot.tracks {
            match track.kind {
                TrackKind::Video => {
                    panel_rows.push(TelemetryPanelRow::normal(format!(
                        "Video: {}",
                        track.codec_id
                    )));
                    if let Some(color_summary) = &track.video_color_summary {
                        panel_rows.push(TelemetryPanelRow::normal(format!(
                            "  Color: {color_summary}"
                        )));
                    }
                }
                TrackKind::Audio => {
                    panel_rows.push(TelemetryPanelRow::normal(format!(
                        "Audio: {}",
                        track.codec_id
                    )));
                }
            };
        }

        if let Some(duration) = player_snapshot.duration {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Duration: {}",
                timeline::format_seconds(Some(duration.as_secs_f64()))
            )));
        }

        if let Some(source_label) = &player_snapshot.media_title {
            panel_rows.push(TelemetryPanelRow::normal(format!("File: {source_label}")));
        }

        panel_rows.push(TelemetryPanelRow::spacer());
        Self::append_packet_rows(panel_rows, telemetry);
        Self::append_timeline_rows(panel_rows, panel_state);
        Self::append_audio_rows(panel_rows, player_snapshot);
        Self::append_video_rows(panel_rows, panel_state);
        Self::append_capability_rows(panel_rows, player_snapshot);
    }

    /// Добавляет packet counters shell telemetry.
    fn append_packet_rows(panel_rows: &mut Vec<TelemetryPanelRow>, telemetry: &Telemetry) {
        let total = telemetry.packets_read();
        let video_packets = telemetry.video_packets();
        let audio_packets = telemetry.audio_packets();

        panel_rows.push(TelemetryPanelRow::normal(format!("Packets: {total}")));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  Video: {video_packets}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  Audio: {audio_packets}"
        )));
    }

    /// Добавляет timeline строки, которые берутся из UI-state/snapshot boundary.
    fn append_timeline_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let playback_position_secs = panel_state
            .timeline_ui_state
            .display_position(&panel_state.player_snapshot.timeline)
            .as_duration()
            .as_secs_f64();

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback PTS: {}",
            timeline::format_seconds(Some(playback_position_secs))
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Timeline: {}",
            timeline::format_seconds(Some(playback_position_secs))
        )));
    }

    /// Добавляет audio diagnostics из `PlayerSnapshot`.
    fn append_audio_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(buffer_level) = player_snapshot.audio_buffer.level {
            let buffer_ms = buffer_level.as_secs_f64() * 1000.0;
            let buffer_tone = if buffer_ms > 10.0 {
                TelemetryPanelRowTone::Good
            } else if buffer_ms > 0.0 {
                TelemetryPanelRowTone::Warning
            } else {
                TelemetryPanelRowTone::Error
            };

            panel_rows.push(TelemetryPanelRow::status(
                format!("Audio buf: {buffer_ms:.1}ms"),
                buffer_tone,
            ));
        }

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Audio underruns: {}",
            player_snapshot.audio_buffer.underruns
        )));
    }

    /// Добавляет video diagnostics без чтения внутренних очередей player-core напрямую.
    fn append_video_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let player_snapshot = panel_state.player_snapshot;
        let telemetry = panel_state.telemetry;
        let diagnostics = &player_snapshot.diagnostics;
        let publish_pressure = diagnostics.decoder_frame_publish_pressure;

        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Video"));

        if let Some(backend_name) = &player_snapshot.active_backend.name {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Backend: {backend_name}"
            )));
        }
        if let Some(active_color_path_text) =
            Self::active_color_path_text_for_ui(panel_state.render_diagnostics)
        {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Color: {active_color_path_text}"
            )));
        }
        if let Some(reference_defaults_text) =
            Self::hdr_reference_defaults_text_for_ui(panel_state.render_diagnostics)
        {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "HDR metadata: {reference_defaults_text}"
            )));
        }

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Decoded: {}",
            telemetry.video_frames_decoded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "video_frames_presented: {}",
            player_snapshot.frame_counters.presented
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback visible drops: {}",
            telemetry.playback_visible_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback drops incl pause: {}",
            player_snapshot.frame_counters.dropped
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_late_drops: {}",
            telemetry.video_late_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_queue_drops: {}",
            telemetry.video_queue_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_pause_drops: {}",
            telemetry.video_pause_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_decoder_starvation: {}",
            telemetry.video_decoder_starvation()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_other_drops: {}",
            telemetry.video_other_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Seek discard, expected: {}",
            telemetry.seek_discarded_frames()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  seek_preroll_discarded: {}",
            telemetry.seek_preroll_discarded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  stale_generation_discarded: {}",
            telemetry.stale_generation_discarded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core seek/pre-roll diagnostics: {}",
            diagnostics.drops.seek_preroll
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core stale-generation diagnostics: {}",
            diagnostics.drops.stale_generation
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core render acquisition timeout: {}",
            diagnostics.drops.render_acquisition_timeout
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core decoder starvation diagnostics: {}",
            diagnostics.drops.decoder_starvation
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "repeated_frames: {}",
            player_snapshot.frame_counters.repeated
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Worker repeats: {}",
            diagnostics.repeated_video_frames
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Decoder publish pressure: {}",
            publish_pressure.frame_publish_channel_full_count
        )));

        Self::append_decoder_control_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Publish max: {:.3}ms",
            publish_pressure
                .max_decoded_frame_publish_latency
                .as_secs_f64()
                * 1000.0
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Frame dur: {:.2}ms",
            panel_state.frame_duration_estimate_ms
        )));

        Self::append_worker_wakeup_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Queue: {}",
            player_snapshot.queues.decoded_video_frames
        )));

        Self::append_texture_pool_rows(panel_rows, player_snapshot);
        Self::append_latency_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Pipeline pauses: {}",
            diagnostics.pauses.total
        )));
    }

    /// Добавляет decoder control-channel строки, если backend их опубликовал.
    fn append_decoder_control_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(control_pressure) = player_snapshot.diagnostics.queues.decoder_control_channel {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Decoder control queue: {}/{}",
                control_pressure.control_channel_len, control_pressure.control_channel_capacity
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Control full/fail: full={} release={} flush={}",
                control_pressure.control_channel_full_count,
                control_pressure.release_control_send_fail_count,
                control_pressure.flush_control_send_fail_count
            )));
        }
    }

    /// Добавляет wakeup/scheduler строки из player diagnostics snapshot.
    fn append_worker_wakeup_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        let worker_wakeup = player_snapshot.diagnostics.worker_wakeup;

        if let Some(reason) = worker_wakeup.reason {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Wake: {}",
                reason.metric_name()
            )));
        }
        if let Some(planned_delay) = worker_wakeup.planned_delay {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Wake delay: {:.2}ms",
                planned_delay.as_secs_f64() * 1000.0
            )));
        }
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Wake late: {:.2}ms",
            worker_wakeup.tick_late_by.as_secs_f64() * 1000.0
        )));
        if let Some(frame_timing) = worker_wakeup.frame_timing {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "PTS-target: {:.2}ms",
                frame_timing.front_frame_delta_from_target_us as f64 / 1000.0
            )));
        }
    }

    /// Добавляет texture-pool строки из backend-neutral diagnostics snapshot.
    fn append_texture_pool_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(texture_pool) = player_snapshot.active_backend.texture_pool {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Textures: {}/{}",
                texture_pool.in_use, texture_pool.capacity
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture slots: {}",
                texture_pool.slots
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture free: {}",
                texture_pool.free_surfaces
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture wait: gpu={} decoder={}",
                texture_pool.waiting_gpu_completion, texture_pool.waiting_decoder_reuse
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Imports: create={} reuse={} replace={} fail={}",
                texture_pool.imports_created,
                texture_pool.imports_reused,
                texture_pool.imports_replaced,
                texture_pool.import_failures
            )));
        }
        if let Some(memory_path) = player_snapshot.diagnostics.zero_copy_memory_path {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Memory path: {memory_path}"
            )));
        }
    }

    /// Добавляет worst-latency строки, сохраняя прежний набор diagnostics.
    fn append_latency_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        let worst_import = player_snapshot
            .diagnostics
            .worst_latencies
            .dma_buf_import
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_import_ms) = worst_import {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst import: {worst_import_ms:.2}ms"
            )));
        }

        let worst_sync = player_snapshot
            .diagnostics
            .worst_latencies
            .hardware_sync
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_sync_ms) = worst_sync {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst sync: {worst_sync_ms:.2}ms"
            )));
        }

        let worst_render_acquire = player_snapshot
            .diagnostics
            .worst_latencies
            .render_acquire
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_render_acquire_ms) = worst_render_acquire {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst acquire: {worst_render_acquire_ms:.3}ms"
            )));
        }

        let render_resource_lock_wait = player_snapshot
            .diagnostics
            .worst_latencies
            .render_resource_lock_wait;
        if render_resource_lock_wait.samples > 0 {
            let average_lock_wait_ms = render_resource_lock_wait.average.as_secs_f64() * 1000.0;
            let worst_lock_wait_ms = render_resource_lock_wait
                .worst
                .map(|sample| sample.duration.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Render resource lock: count={} avg={average_lock_wait_ms:.3}ms max={worst_lock_wait_ms:.3}ms",
                render_resource_lock_wait.samples
            )));
        }
        if player_snapshot.diagnostics.render_resource_lock_busy_count > 0 {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Render resource busy: count={} reuse={}",
                player_snapshot.diagnostics.render_resource_lock_busy_count,
                player_snapshot
                    .diagnostics
                    .render_resource_previous_frame_reuse_count
            )));
        }
    }

    /// Добавляет capability summary построчно, чтобы virtualization могла скрыть offscreen строки.
    fn append_capability_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(capability_summary) = &player_snapshot.capability_summary {
            panel_rows.push(TelemetryPanelRow::spacer());
            panel_rows.push(TelemetryPanelRow::heading("Capabilities"));
            for line in capability_summary.lines() {
                panel_rows.push(TelemetryPanelRow::normal(line));
            }
        }
    }

    /// Формирует UI-строку active color path из renderer-neutral diagnostics.
    fn active_color_path_text_for_ui(render_diagnostics: &RenderDiagnostics) -> Option<String> {
        render_diagnostics.active_color_path_text()
    }

    /// Формирует UI-строку source markers optional HDR metadata.
    fn hdr_reference_defaults_text_for_ui(
        render_diagnostics: &RenderDiagnostics,
    ) -> Option<String> {
        render_diagnostics.hdr_reference_defaults_text()
    }

    /// Рендерит центральный overlay состояния.
    fn render_center_overlay(
        ui: &mut egui::Ui,
        is_playing: bool,
        error_message: Option<&str>,
        pending_message: Option<&str>,
        skin: &impl PlayerSkin,
        animation_state: AnimationState,
    ) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                if let Some(dim_color) = skin.stale_frame_dim_color(animation_state) {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, dim_color);
                }

                if let Some(error) = error_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::RED, error);
                    });
                } else if let Some(message) = pending_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::LIGHT_BLUE, message);
                    });
                } else if !is_playing {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading("Press Play to start");
                    });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use media_core::MediaTime;
    use player_core::{
        MediaOpenRequest, MediaSource, MediaSummary, PlaybackResumeIntent, PlaybackState,
        PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerSnapshot, SeekCommitInfo,
        SeekRequest,
    };
    use render_core::{
        ActiveColorPath, ColorPipelineSettings, HdrMetadataDiagnosticMarker,
        HdrReferenceDefaultDiagnostics, RenderDiagnostics, VideoFrameFormat,
    };
    use video_core::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameResourceHandle};

    use super::{
        AppFrameContext, AppState, CachedPresentFrameDiscardReason,
        CachedPresentFrameValidationState, PresentFrameIdentity, TELEMETRY_PANEL_REFRESH_INTERVAL,
        TelemetryPanelCache, TelemetryPanelRow, TelemetryPanelState,
        TextureBusyFallbackRejectReason, TextureBusyFallbackReuseState,
        cached_present_frame_discard_reason_for_player_event, cached_present_frame_stale_reason,
        texture_busy_fallback_can_reuse_previous_frame, texture_busy_fallback_reject_reason,
        timeline_command_from_action,
    };
    use crate::telemetry::Telemetry;
    use crate::ui::timeline::{TimelineAction, TimelineUiState};

    /// Возвращает участок source между двумя маркерами для architecture guard tests.
    fn source_section_between<'source>(
        source_code: &'source str,
        start_marker: &str,
        end_marker: &str,
    ) -> &'source str {
        let section_start = source_code
            .find(start_marker)
            .unwrap_or_else(|| panic!("Не найден начальный source marker: {start_marker}"));
        let section_after_start = &source_code[section_start..];
        let section_end = section_after_start
            .find(end_marker)
            .unwrap_or_else(|| panic!("Не найден конечный source marker: {end_marker}"));

        &section_after_start[..section_end]
    }

    /// Создаёт decoded frame для pure identity tests без GPU lease-а.
    fn decoded_frame_for_identity_tests(
        generation: u64,
        pts: Duration,
        resource_handle: u64,
    ) -> DecodedFrame {
        DecodedFrame {
            generation,
            pts,
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(resource_handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Создаёт минимальный input telemetry panel без запуска worker/renderer.
    fn telemetry_panel_state_for_tests<'state>(
        player_snapshot: &'state PlayerSnapshot,
        telemetry: &'state Telemetry,
        render_diagnostics: &'state RenderDiagnostics,
        timeline_ui_state: &'state TimelineUiState,
        start_time: Instant,
    ) -> TelemetryPanelState<'state> {
        TelemetryPanelState {
            player_snapshot,
            telemetry,
            render_diagnostics,
            timeline_ui_state,
            backend_name: "test-backend",
            start_time,
            frame_duration_estimate_ms: 16.67,
        }
    }

    /// Ищет точный текст в cached строках telemetry panel.
    fn telemetry_rows_contain(panel_rows: &[TelemetryPanelRow], expected_text: &str) -> bool {
        panel_rows
            .iter()
            .any(|panel_row| panel_row.text() == expected_text)
    }

    /// Проверяет, что click-to-seek уходит в exact seek route без scrub policy.
    #[test]
    fn timeline_click_seek_maps_to_exact_player_seek_route() {
        let target_position = MediaTime::from_secs(42);

        let (command, route) =
            timeline_command_from_action(TimelineAction::ClickSeek(target_position));

        assert_eq!(route.as_str(), "click-seek");
        assert_eq!(
            command,
            PlayerCommand::Seek(SeekRequest::absolute(target_position))
        );
    }

    /// Проверяет, что drag release уходит в exact seek route без scrub policy.
    #[test]
    fn timeline_drag_release_maps_to_exact_player_seek_route() {
        let target_position = MediaTime::from_secs(64);

        let (command, route) =
            timeline_command_from_action(TimelineAction::CommitDragSeek(target_position));

        assert_eq!(route.as_str(), "drag-seek");
        assert_eq!(
            command,
            PlayerCommand::Seek(SeekRequest::absolute(target_position))
        );
    }

    /// Проверяет, что UI diagnostics получает active path как renderer-neutral данные.
    #[test]
    fn ui_diagnostics_reads_active_color_path_without_gpu_handles() {
        let settings = ColorPipelineSettings::default();
        let active_path = ActiveColorPath::from_parts(
            VideoFrameFormat::Nv12,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            VideoColorMetadata::sdr_bt709_limited(),
            &settings,
        );
        let render_diagnostics = RenderDiagnostics {
            active_color_path: Some(active_path),
            hdr_reference_defaults: Some(HdrReferenceDefaultDiagnostics {
                mastering_max_luminance: HdrMetadataDiagnosticMarker::Confirmed,
                mastering_min_luminance: HdrMetadataDiagnosticMarker::Confirmed,
                max_content_light_level: HdrMetadataDiagnosticMarker::ReferenceDefault,
                max_frame_average_light_level: HdrMetadataDiagnosticMarker::ReferenceDefault,
            }),
        };

        assert_eq!(
            AppState::active_color_path_text_for_ui(&render_diagnostics).as_deref(),
            Some("NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm")
        );
        assert_eq!(
            AppState::hdr_reference_defaults_text_for_ui(&render_diagnostics).as_deref(),
            Some(
                "mastering-max=confirmed, mastering-min=confirmed, maxcll=reference-default, maxfall=reference-default"
            )
        );
    }

    /// Проверяет, что telemetry panel не форматирует heavy diagnostics чаще refresh interval.
    #[test]
    fn telemetry_panel_cache_reuses_rows_until_refresh_deadline() {
        let mut telemetry_panel_cache = TelemetryPanelCache::default();
        let player_snapshot = PlayerSnapshot::empty();
        let telemetry = Telemetry::new();
        let render_diagnostics = RenderDiagnostics::default();
        let timeline_ui_state = TimelineUiState::default();
        let started_at = Instant::now();

        let initial_rows = telemetry_panel_cache.rows_for_frame(
            started_at,
            telemetry_panel_state_for_tests(
                &player_snapshot,
                &telemetry,
                &render_diagnostics,
                &timeline_ui_state,
                started_at,
            ),
        );
        assert!(telemetry_rows_contain(
            &initial_rows,
            "frames_presented_to_surface: 0"
        ));

        telemetry.record_frame_presented_to_surface();
        let rows_before_deadline = telemetry_panel_cache.rows_for_frame(
            started_at + TELEMETRY_PANEL_REFRESH_INTERVAL / 2,
            telemetry_panel_state_for_tests(
                &player_snapshot,
                &telemetry,
                &render_diagnostics,
                &timeline_ui_state,
                started_at,
            ),
        );
        assert!(Arc::ptr_eq(&initial_rows, &rows_before_deadline));
        assert!(telemetry_rows_contain(
            &rows_before_deadline,
            "frames_presented_to_surface: 0"
        ));

        let rows_after_deadline = telemetry_panel_cache.rows_for_frame(
            started_at + TELEMETRY_PANEL_REFRESH_INTERVAL,
            telemetry_panel_state_for_tests(
                &player_snapshot,
                &telemetry,
                &render_diagnostics,
                &timeline_ui_state,
                started_at,
            ),
        );
        assert!(!Arc::ptr_eq(&initial_rows, &rows_after_deadline));
        assert!(telemetry_rows_contain(
            &rows_after_deadline,
            "frames_presented_to_surface: 1"
        ));
    }

    /// Проверяет empty snapshot path без player/render side effects.
    #[test]
    fn telemetry_panel_rows_keep_empty_media_state_explicit() {
        let player_snapshot = PlayerSnapshot::empty();
        let telemetry = Telemetry::new();
        let render_diagnostics = RenderDiagnostics::default();
        let timeline_ui_state = TimelineUiState::default();
        let started_at = Instant::now();

        let panel_rows = AppState::build_telemetry_panel_rows(telemetry_panel_state_for_tests(
            &player_snapshot,
            &telemetry,
            &render_diagnostics,
            &timeline_ui_state,
            started_at,
        ));

        assert!(telemetry_rows_contain(&panel_rows, "[Media Info]"));
        assert!(telemetry_rows_contain(&panel_rows, "No file loaded"));
        assert!(!telemetry_rows_contain(&panel_rows, "[Video]"));
    }

    /// Проверяет, что app shell не читает внутренний present frame из player pipeline.
    #[test]
    fn app_egui_does_not_access_pipeline_present_video_frame_directly() {
        let forbidden_member = concat!("pipeline", ".", "present_video_frame");

        assert!(!include_str!("state.rs").contains(forbidden_member));
        assert!(!include_str!("main.rs").contains(forbidden_member));
        assert!(!include_str!("app_shell/mod.rs").contains(forbidden_member));
    }

    /// Фиксирует явную границу refresh/publish вместо getter-like API с side effects.
    #[test]
    fn app_state_player_snapshot_boundary_stays_explicit() {
        let state_source = include_str!("state.rs");
        let frame_prepare_source = include_str!("frame_prepare.rs");
        let removed_getter_signature = concat!("fn ", "player_snapshot", "(&mut self)");
        let refresh_signature = concat!(
            "pub(crate) fn ",
            "refresh_player_snapshot",
            "(&mut self) -> PlayerSnapshot"
        );
        let publish_signature = concat!(
            "pub(crate) fn ",
            "publish_desktop_snapshot",
            "(&self, player_snapshot: &PlayerSnapshot)"
        );

        assert!(
            !state_source.contains(removed_getter_signature),
            "AppState не должен возвращать player snapshot через mutable getter-like API"
        );
        assert!(
            state_source.contains(refresh_signature),
            "AppState должен явно читать worker snapshot через refresh_player_snapshot()"
        );
        assert!(
            state_source.contains(publish_signature),
            "AppState должен явно публиковать desktop snapshot через publish_desktop_snapshot()"
        );

        let refresh_section = source_section_between(
            state_source,
            refresh_signature,
            concat!("pub fn ", "wants_continuous_redraw", "(&self) -> bool"),
        );
        assert!(
            !refresh_section.contains("publish_desktop_snapshot"),
            "refresh_player_snapshot() не должен публиковать desktop state"
        );
        assert!(
            !refresh_section.contains("desktop_integration"),
            "refresh_player_snapshot() не должен напрямую трогать desktop integration"
        );

        let publish_section = source_section_between(
            state_source,
            publish_signature,
            concat!("fn ", "log_desktop_integration_events"),
        );
        assert!(
            !publish_section.contains("latest_snapshot"),
            "publish_desktop_snapshot() не должен читать worker snapshot"
        );
        assert!(
            !publish_section.contains("player_worker"),
            "publish_desktop_snapshot() не должен зависеть от worker storage"
        );

        let begin_frame_position = frame_prepare_source
            .find("let frame_context = app_state.begin_frame_context(renderer.diagnostics());")
            .expect("render_frame должен создавать AppFrameContext перед публикацией");
        let publish_position = frame_prepare_source
            .find("app_state.publish_desktop_snapshot(frame_context.player_snapshot());")
            .expect("render_frame должен явно публиковать snapshot текущего frame-а");
        let ui_prepare_position = frame_prepare_source
            .find("let prepared_ui_frame = prepare_ui_frame(window, app_state, egui_input, &frame_context);")
            .expect("render_frame должен готовить UI через тот же AppFrameContext");

        assert!(
            begin_frame_position < publish_position && publish_position < ui_prepare_position,
            "render_frame должен публиковать snapshot из AppFrameContext до UI/render подготовки"
        );
    }

    /// Проверяет, что AppFrameContext отдаёт уже зафиксированный snapshot по ссылке.
    #[test]
    fn app_frame_context_returns_fixed_player_snapshot_reference() {
        let frame_context = AppFrameContext {
            player_snapshot: PlayerSnapshot::empty(),
            render_diagnostics: RenderDiagnostics::default(),
        };

        assert!(std::ptr::eq(
            frame_context.player_snapshot(),
            &frame_context.player_snapshot
        ));
    }

    /// Проверяет pure-classifier stale cache без создания GPU lease-а.
    #[test]
    fn cached_present_frame_validation_rejects_stale_lifecycle_identity() {
        let valid_state = CachedPresentFrameValidationState {
            current_video_frame_present: true,
            source_matches: true,
            cached_generation: 7,
            current_generation: 7,
        };

        assert_eq!(cached_present_frame_stale_reason(valid_state), None);
        assert_eq!(
            cached_present_frame_stale_reason(CachedPresentFrameValidationState {
                current_video_frame_present: false,
                ..valid_state
            }),
            Some(CachedPresentFrameDiscardReason::CurrentVideoFrameMissing)
        );
        assert_eq!(
            cached_present_frame_stale_reason(CachedPresentFrameValidationState {
                source_matches: false,
                ..valid_state
            }),
            Some(CachedPresentFrameDiscardReason::SourceLabelChanged)
        );
        assert_eq!(
            cached_present_frame_stale_reason(CachedPresentFrameValidationState {
                current_generation: 8,
                ..valid_state
            }),
            Some(CachedPresentFrameDiscardReason::RenderGenerationChanged)
        );
    }

    /// Проверяет, что player lifecycle events инвалидируют cache только на boundary-событиях.
    #[test]
    fn player_lifecycle_events_invalidate_cached_present_frame_at_boundaries() {
        let media_open_request =
            MediaOpenRequest::new(MediaSource::ExternalLabel("next-source".to_string()), true);
        let media_summary = MediaSummary {
            title: None,
            source_label: "next-source".to_string(),
            duration: None,
        };
        let fatal_error = PlayerError::new(PlayerErrorKind::RenderDeviceLost, "device lost");

        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(&PlayerEvent::MediaOpenRequested(
                media_open_request
            )),
            Some(CachedPresentFrameDiscardReason::PlayerMediaOpenRequested)
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(&PlayerEvent::MediaOpened(
                media_summary
            )),
            Some(CachedPresentFrameDiscardReason::PlayerMediaOpened)
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(
                &PlayerEvent::PlaybackStateChanged(PlaybackState::Stopped)
            ),
            Some(CachedPresentFrameDiscardReason::PlayerStopped)
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(
                &PlayerEvent::PlaybackStateChanged(PlaybackState::Failed)
            ),
            Some(CachedPresentFrameDiscardReason::PlayerFailed)
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(&PlayerEvent::FatalError(
                fatal_error
            )),
            Some(CachedPresentFrameDiscardReason::PlayerFatalError)
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(
                &PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)
            ),
            None
        );
        assert_eq!(
            cached_present_frame_discard_reason_for_player_event(&PlayerEvent::SeekCommitted(
                SeekCommitInfo {
                    target_position: Duration::from_secs(12),
                    actual_position: Duration::from_secs(12),
                    resume_intent: PlaybackResumeIntent::Pause,
                }
            )),
            None
        );
    }

    /// Проверяет, что reuse identity различает новый decoded frame на той же texture.
    #[test]
    fn present_frame_identity_distinguishes_decoded_generation_and_pts() {
        let previous_frame = decoded_frame_for_identity_tests(10, Duration::from_millis(1_000), 42);
        let next_generation_frame =
            decoded_frame_for_identity_tests(11, Duration::from_millis(1_000), 42);
        let next_pts_frame = decoded_frame_for_identity_tests(10, Duration::from_millis(1_033), 42);

        let previous_identity = PresentFrameIdentity::from_decoded_frame(7, &previous_frame);

        assert_ne!(
            previous_identity,
            PresentFrameIdentity::from_decoded_frame(7, &next_generation_frame)
        );
        assert_ne!(
            previous_identity,
            PresentFrameIdentity::from_decoded_frame(7, &next_pts_frame)
        );
    }

    /// Проверяет pure decision для texture-view Busy fallback-а.
    #[test]
    fn texture_busy_fallback_reuses_valid_previous_frame() {
        let valid_previous_frame = TextureBusyFallbackReuseState {
            cached_generation: 5,
            current_generation: 5,
            source_matches: true,
            has_current_video_frame: true,
            cached_frame_is_stale: false,
            timeline_marks_frame_stale: false,
        };

        assert!(texture_busy_fallback_can_reuse_previous_frame(
            valid_previous_frame
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(valid_previous_frame),
            None
        );
    }

    /// Проверяет, что новый pending scrub target не запрещает reuse валидного previous frame-а.
    #[test]
    fn texture_busy_fallback_reuses_previous_frame_while_target_is_pending() {
        let valid_previous_frame_with_pending_target = TextureBusyFallbackReuseState {
            cached_generation: 5,
            current_generation: 5,
            source_matches: true,
            has_current_video_frame: true,
            cached_frame_is_stale: false,
            timeline_marks_frame_stale: false,
        };

        assert!(texture_busy_fallback_can_reuse_previous_frame(
            valid_previous_frame_with_pending_target
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(valid_previous_frame_with_pending_target),
            None
        );
    }

    /// Проверяет, что старый pre-scrub frame не маскируется как fresh при busy texture lock-е.
    #[test]
    fn texture_busy_fallback_rejects_stale_pre_scrub_frame_during_seek() {
        let stale_pre_scrub_frame = TextureBusyFallbackReuseState {
            cached_generation: 5,
            current_generation: 5,
            source_matches: true,
            has_current_video_frame: true,
            cached_frame_is_stale: false,
            timeline_marks_frame_stale: true,
        };

        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            stale_pre_scrub_frame
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(stale_pre_scrub_frame),
            Some(TextureBusyFallbackRejectReason::TimelineFrameStale)
        );
    }

    /// Проверяет, что Busy fallback различает lifecycle причины отказа.
    #[test]
    fn texture_busy_fallback_rejects_stale_lifecycle_identity() {
        let valid_previous_frame = TextureBusyFallbackReuseState {
            cached_generation: 5,
            current_generation: 5,
            source_matches: true,
            has_current_video_frame: true,
            cached_frame_is_stale: false,
            timeline_marks_frame_stale: false,
        };

        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            TextureBusyFallbackReuseState {
                current_generation: 6,
                ..valid_previous_frame
            }
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
                current_generation: 6,
                ..valid_previous_frame
            }),
            Some(TextureBusyFallbackRejectReason::RenderGenerationChanged)
        );
        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            TextureBusyFallbackReuseState {
                source_matches: false,
                ..valid_previous_frame
            }
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
                source_matches: false,
                ..valid_previous_frame
            }),
            Some(TextureBusyFallbackRejectReason::SourceLabelChanged)
        );
        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            TextureBusyFallbackReuseState {
                has_current_video_frame: false,
                ..valid_previous_frame
            }
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
                has_current_video_frame: false,
                ..valid_previous_frame
            }),
            Some(TextureBusyFallbackRejectReason::CurrentVideoFrameMissing)
        );
        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            TextureBusyFallbackReuseState {
                cached_frame_is_stale: true,
                ..valid_previous_frame
            }
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
                cached_frame_is_stale: true,
                ..valid_previous_frame
            }),
            Some(TextureBusyFallbackRejectReason::CachedFrameStale)
        );
        assert!(!texture_busy_fallback_can_reuse_previous_frame(
            TextureBusyFallbackReuseState {
                timeline_marks_frame_stale: true,
                ..valid_previous_frame
            }
        ));
        assert_eq!(
            texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
                timeline_marks_frame_stale: true,
                ..valid_previous_frame
            }),
            Some(TextureBusyFallbackRejectReason::TimelineFrameStale)
        );
    }
}
