/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use capability_core::SystemCapabilities;
use desktop_integration::{DesktopIntegration, DesktopIntegrationEvent};
use media_core::TrackKind;
use player_core::{
    FrameCounters, PlaybackState, PlayerCommand, PlayerPresentFrame, PlayerRenderError,
    PlayerSnapshot, PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent, ScrubCommitPolicy,
    SeekRequest,
};
use render_core::RenderDiagnostics;
use rustiplayer_config::AppConfig;
use tracing::{debug, instrument, warn};
use winit::window::Window;

use crate::telemetry::Telemetry;
use crate::ui::animation::AnimationState;
use crate::ui::player_controls::{self, ControlAction};
use crate::ui::skin::{self, PlayerSkin};
use crate::ui::timeline::{self, TimelineAction, TimelineUiState};

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

    /// Последняя renderer-neutral диагностика без GPU handles.
    render_diagnostics: RenderDiagnostics,

    /// Последний snapshot, уже доставленный UI; используется shell redraw pacing-ом.
    last_player_snapshot: PlayerSnapshot,

    /// Нужен один follow-up redraw после команды, которая уйдёт в worker асинхронно.
    pending_redraw_after_worker_command: bool,

    /// Последний валидный кадр, который можно повторить при transient miss render boundary.
    cached_present_frame: Option<PlayerPresentFrame>,

    /// Source label, которому принадлежит cached present frame.
    cached_present_source_label: Option<String>,

    /// Последний локальный файл, открытый shell-ом, для восстановления после suspend.
    current_local_file: Option<PathBuf>,

    /// Transient pointer state timeline; player position здесь не хранится.
    timeline_ui_state: TimelineUiState,

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
        let worker_config = PlayerWorkerConfig::from_app_config(&app_config);
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
            render_diagnostics: RenderDiagnostics::default(),
            last_player_snapshot: PlayerSnapshot::empty(),
            pending_redraw_after_worker_command: false,
            cached_present_frame: None,
            cached_present_source_label: None,
            current_local_file: None,
            timeline_ui_state: TimelineUiState::default(),
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

    /// Обновляет renderer diagnostics, которые UI покажет в telemetry panel.
    pub fn set_render_diagnostics(&mut self, render_diagnostics: RenderDiagnostics) {
        self.render_diagnostics = render_diagnostics;
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

    /// Возвращает read-only snapshot из `player-core` для UI и renderer diagnostics.
    #[must_use]
    pub fn player_snapshot(&mut self) -> PlayerSnapshot {
        let player_snapshot = self
            .player_worker
            .latest_snapshot(self.frame_counters_snapshot());
        self.last_player_snapshot = player_snapshot.clone();
        self.publish_desktop_snapshot(&player_snapshot);
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

    /// Публикует read-only snapshot в desktop integration boundary.
    fn publish_desktop_snapshot(&self, player_snapshot: &PlayerSnapshot) {
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
        self.clear_cached_present_frame();
        self.clear_startup_status();
        self.current_local_file = Some(path.to_path_buf());
        if let Err(error) = self.player_worker.load_file(path, autoplay) {
            warn!(error = %error, "Не удалось отправить команду открытия файла в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Загружает YouTube demuxer без долговременного database/cache слоя.
    pub fn load_youtube_demuxer(
        &mut self,
        label: String,
        demuxer: Box<dyn webm_demux::Demuxer + Send>,
    ) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame();
        self.clear_startup_status();
        self.current_local_file = None;
        if let Err(error) = self.player_worker.load_demuxer(label, demuxer, autoplay) {
            warn!(error = %error, "Не удалось отправить YouTube demuxer в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Инициализирует video pipeline в playback worker.
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        if let Err(error) = self
            .player_worker
            .init_video_pipeline(instance, adapter, device, queue)
        {
            warn!(error = %error, "Не удалось отправить init video pipeline в worker");
            return;
        }

        self.mark_pending_worker_redraw();
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
    pub fn acquire_present_frame_for_render(&mut self) -> PresentFrameAcquisition {
        let player_snapshot = self.player_snapshot();
        let rejected_stale_cached_frame = self.drop_stale_cached_present_frame(&player_snapshot);

        if let Some(mut present_frame) = self.player_worker.try_acquire_present_frame() {
            if present_frame.render_generation != player_snapshot.render_generation {
                self.clear_cached_present_frame();
                return PresentFrameAcquisition::StaleFrameRejected;
            }

            present_frame.stale = present_frame.stale || player_snapshot.timeline.stale_frame;
            let cached_frame_identity = self
                .cached_present_frame
                .as_ref()
                .map(Self::present_frame_identity);
            let acquired_frame_identity = Self::present_frame_identity(&present_frame);

            if cached_frame_identity == Some(acquired_frame_identity) {
                return self
                    .cached_present_frame
                    .clone()
                    .map(|mut cached_present_frame| {
                        cached_present_frame.stale = present_frame.stale;
                        PresentFrameAcquisition::ReusedPreviousFrame(cached_present_frame)
                    })
                    .unwrap_or(PresentFrameAcquisition::NoFrameYet);
            }

            self.cached_present_source_label = player_snapshot.source_label.clone();
            self.cached_present_frame = Some(present_frame.clone());
            return PresentFrameAcquisition::NewFrameAcquired(present_frame);
        }

        if let Some(mut cached_present_frame) = self.cached_present_frame.clone() {
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
        if self.cached_present_frame.is_none() {
            return false;
        }

        if player_snapshot.current_video_frame.is_none() {
            self.cached_present_frame = None;
            self.cached_present_source_label = None;
            return false;
        }

        if self.cached_present_source_label != player_snapshot.source_label {
            self.cached_present_frame = None;
            self.cached_present_source_label = None;
            return false;
        }

        let Some(cached_present_frame) = &self.cached_present_frame else {
            return false;
        };
        if cached_present_frame.render_generation != player_snapshot.render_generation {
            self.clear_cached_present_frame();
            return true;
        }

        false
    }

    /// Возвращает identity frame-а, достаточную для отличия нового lease-а от reuse.
    fn present_frame_identity(present_frame: &PlayerPresentFrame) -> (u64, u64) {
        (
            present_frame.render_generation,
            present_frame.frame.texture_handle.0,
        )
    }

    /// Освобождает cached present frame и отправляет drop-ack worker-у через lease guard.
    fn clear_cached_present_frame(&mut self) {
        self.cached_present_frame = None;
        self.cached_present_source_label = None;
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

    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure отправляет worker-у.
    #[instrument(skip(self, window))]
    pub fn render_ui(&mut self, window: &Window, egui_input: egui::RawInput) -> egui::FullOutput {
        let player_snapshot = self.player_snapshot();
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
        let render_diagnostics = self.render_diagnostics.clone();
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

        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            player_controls::render_top_bar(ui, app_version, &selected_skin);
            control_actions = player_controls::render_bottom_controls(
                ui,
                &player_snapshot,
                &mut timeline_ui_state,
                &selected_skin,
            );

            if show_telemetry {
                Self::render_telemetry_panel(
                    ui,
                    TelemetryPanelState {
                        player_snapshot: &player_snapshot,
                        telemetry: &telemetry,
                        render_diagnostics: &render_diagnostics,
                        timeline_ui_state: &timeline_ui_state,
                        backend_name: &backend_name,
                        start_time,
                        frame_duration_estimate_ms,
                    },
                );
            }
            Self::render_center_overlay(
                ui,
                is_playing,
                error_message,
                pending_message,
                &selected_skin,
                animation_state,
            );
        });

        self.timeline_ui_state = timeline_ui_state;
        self.handle_control_actions(window, control_actions);

        full_output
    }

    /// Применяет действия controls после завершения egui pass.
    fn handle_control_actions(&mut self, window: &Window, actions: Vec<ControlAction>) {
        for action in actions {
            match action {
                ControlAction::TogglePlayback => self.toggle_playback(),
                ControlAction::OpenFile => self.open_file(),
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

    /// Конвертирует timeline action в typed player command.
    fn send_timeline_action(&mut self, action: TimelineAction) {
        debug!(action = ?action, "Timeline action отправлен в player worker");
        let command = match action {
            TimelineAction::BeginScrub => PlayerCommand::BeginScrub,
            TimelineAction::UpdateScrub(position) => {
                PlayerCommand::UpdateScrub(SeekRequest::absolute(position))
            }
            TimelineAction::EndScrubCommitDefault => PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            },
        };

        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось отправить timeline command");
            return;
        }

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
                let current_volume = self.player_snapshot().volume;
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

    /// Открывает WebM/MKV файл через file dialog.
    pub fn open_file(&mut self) {
        let file = rfd::FileDialog::new()
            .add_filter("WebM Video", &["webm"])
            .add_filter("Matroska Video", &["mkv"])
            .add_filter("All Files", &["*"])
            .pick_file();

        if let Some(path) = file {
            self.load_file(&path);
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

    /// Рендерит правую диагностическую панель на основе snapshot.
    fn render_telemetry_panel(ui: &mut egui::Ui, panel_state: TelemetryPanelState<'_>) {
        let TelemetryPanelState {
            player_snapshot,
            telemetry,
            render_diagnostics,
            timeline_ui_state,
            backend_name,
            start_time,
            frame_duration_estimate_ms,
        } = panel_state;
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
                    .show(ui, |ui| {
                        let fps = telemetry.current_fps();
                        let frame_time = telemetry.last_frame_time_ms();
                        let presented = telemetry.presented_frames();
                        let dropped = telemetry.dropped_frames();
                        let drop_rate = telemetry.drop_rate_percent();

                        ui.monospace(format!("FPS: {fps}"));
                        ui.monospace(format!("Frame time: {:.2} ms", frame_time as f64));
                        ui.separator();
                        ui.monospace(format!("Presented: {presented}"));
                        ui.monospace(format!("Dropped: {dropped}"));
                        ui.monospace(format!("Drop rate: {drop_rate:.2}%"));
                        ui.separator();

                        let quality_color = if fps >= 55 {
                            egui::Color32::GREEN
                        } else if fps >= 30 {
                            egui::Color32::YELLOW
                        } else {
                            egui::Color32::RED
                        };
                        ui.colored_label(quality_color, "Frame pacing: OK");

                        ui.separator();
                        ui.monospace(format!("Backend: {backend_name}"));
                        ui.monospace(format!(
                            "Elapsed: {:.1}s",
                            start_time.elapsed().as_secs_f64()
                        ));

                        ui.separator();
                        ui.heading("Media Info");
                        Self::render_media_info(
                            ui,
                            player_snapshot,
                            telemetry,
                            render_diagnostics,
                            timeline_ui_state,
                            frame_duration_estimate_ms,
                        );
                    });
            });
    }

    /// Рендерит media diagnostics из `PlayerSnapshot`.
    fn render_media_info(
        ui: &mut egui::Ui,
        player_snapshot: &PlayerSnapshot,
        telemetry: &Telemetry,
        render_diagnostics: &RenderDiagnostics,
        timeline_ui_state: &TimelineUiState,
        frame_duration_estimate_ms: f64,
    ) {
        if player_snapshot.source_label.is_none() {
            ui.monospace("No file loaded");
            return;
        }

        for track in &player_snapshot.tracks {
            match track.kind {
                TrackKind::Video => {
                    ui.monospace(format!("Video: {}", track.codec_id));
                    if let Some(color_summary) = &track.video_color_summary {
                        ui.monospace(format!("  Color: {color_summary}"));
                    }
                }
                TrackKind::Audio => {
                    ui.monospace(format!("Audio: {}", track.codec_id));
                }
            };
        }

        if let Some(duration) = player_snapshot.duration {
            ui.monospace(format!(
                "Duration: {}",
                timeline::format_seconds(Some(duration.as_secs_f64()))
            ));
        }

        if let Some(source_label) = &player_snapshot.media_title {
            ui.monospace(format!("File: {source_label}"));
        }

        ui.separator();
        let total = telemetry.packets_read();
        let video_packets = telemetry.video_packets();
        let audio_packets = telemetry.audio_packets();
        ui.monospace(format!("Packets: {total}"));
        ui.monospace(format!("  Video: {video_packets}"));
        ui.monospace(format!("  Audio: {audio_packets}"));

        let playback_position_secs = timeline_ui_state
            .display_position(&player_snapshot.timeline)
            .as_duration()
            .as_secs_f64();
        ui.monospace(format!(
            "Playback PTS: {}",
            timeline::format_seconds(Some(playback_position_secs))
        ));

        ui.separator();
        ui.monospace(format!(
            "Timeline: {}",
            timeline::format_seconds(Some(playback_position_secs))
        ));
        if let Some(buffer_level) = player_snapshot.audio_buffer.level {
            let buffer_ms = buffer_level.as_secs_f64() * 1000.0;
            let buffer_color = if buffer_ms > 10.0 {
                egui::Color32::GREEN
            } else if buffer_ms > 0.0 {
                egui::Color32::YELLOW
            } else {
                egui::Color32::RED
            };
            ui.colored_label(buffer_color, format!("Audio buf: {buffer_ms:.1}ms"));
        }
        ui.monospace(format!(
            "Audio underruns: {}",
            player_snapshot.audio_buffer.underruns
        ));

        ui.separator();
        ui.heading("Video");
        if let Some(backend_name) = &player_snapshot.active_backend.name {
            ui.monospace(format!("Backend: {backend_name}"));
        }
        if let Some(active_color_path_text) =
            Self::active_color_path_text_for_ui(render_diagnostics)
        {
            ui.monospace(format!("Color: {active_color_path_text}"));
        }
        if let Some(reference_defaults_text) =
            Self::hdr_reference_defaults_text_for_ui(render_diagnostics)
        {
            ui.monospace(format!("HDR metadata: {reference_defaults_text}"));
        }
        ui.monospace(format!("Decoded: {}", telemetry.video_frames_decoded()));
        ui.monospace(format!(
            "Presented: {}",
            player_snapshot.frame_counters.presented
        ));
        ui.monospace(format!(
            "Dropped: {}",
            player_snapshot.frame_counters.dropped
        ));
        ui.monospace(format!("  Late: {}", telemetry.video_frames_late_dropped()));
        ui.monospace(format!(
            "  Queue: {}",
            telemetry.video_frames_queue_dropped()
        ));
        ui.monospace(format!(
            "  Pause: {}",
            telemetry.video_frames_pause_dropped()
        ));
        ui.monospace(format!(
            "  Other: {}",
            telemetry.video_frames_other_dropped()
        ));
        ui.monospace(format!(
            "Seek discarded: {}",
            telemetry.seek_discarded_frames()
        ));
        ui.monospace(format!(
            "  Seek/pre-roll: {}",
            player_snapshot.diagnostics.drops.seek_preroll
        ));
        ui.monospace(format!(
            "  Stale gen: {}",
            player_snapshot.diagnostics.drops.stale_generation
        ));
        ui.monospace(format!(
            "  Acquire timeout (legacy): {}",
            player_snapshot.diagnostics.drops.render_acquisition_timeout
        ));
        ui.monospace(format!(
            "  Decoder starvation: {}",
            player_snapshot.diagnostics.drops.decoder_starvation
        ));
        ui.monospace(format!(
            "Repeated/reused: {}",
            player_snapshot.frame_counters.repeated
        ));
        ui.monospace(format!(
            "Worker repeats: {}",
            player_snapshot.diagnostics.repeated_video_frames
        ));
        ui.monospace(format!("Frame dur: {:.2}ms", frame_duration_estimate_ms));
        let worker_wakeup = player_snapshot.diagnostics.worker_wakeup;
        if let Some(reason) = worker_wakeup.reason {
            ui.monospace(format!("Wake: {}", reason.metric_name()));
        }
        if let Some(planned_delay) = worker_wakeup.planned_delay {
            ui.monospace(format!(
                "Wake delay: {:.2}ms",
                planned_delay.as_secs_f64() * 1000.0
            ));
        }
        ui.monospace(format!(
            "Wake late: {:.2}ms",
            worker_wakeup.tick_late_by.as_secs_f64() * 1000.0
        ));
        if let Some(frame_timing) = worker_wakeup.frame_timing {
            ui.monospace(format!(
                "PTS-target: {:.2}ms",
                frame_timing.front_frame_delta_from_target_us as f64 / 1000.0
            ));
        }
        ui.monospace(format!(
            "Queue: {}",
            player_snapshot.queues.decoded_video_frames
        ));
        if let Some(texture_pool) = player_snapshot.active_backend.texture_pool {
            ui.monospace(format!(
                "Textures: {}/{}",
                texture_pool.in_use, texture_pool.capacity
            ));
            ui.monospace(format!("Texture slots: {}", texture_pool.slots));
            ui.monospace(format!("Texture free: {}", texture_pool.free_surfaces));
            ui.monospace(format!(
                "Texture wait: gpu={} decoder={}",
                texture_pool.waiting_gpu_completion, texture_pool.waiting_decoder_reuse
            ));
            ui.monospace(format!(
                "Imports: create={} reuse={} replace={} fail={}",
                texture_pool.imports_created,
                texture_pool.imports_reused,
                texture_pool.imports_replaced,
                texture_pool.import_failures
            ));
        }
        if let Some(memory_path) = player_snapshot.diagnostics.zero_copy_memory_path {
            ui.monospace(format!("Memory path: {memory_path}"));
        }
        let worst_import = player_snapshot
            .diagnostics
            .worst_latencies
            .dma_buf_import
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_import_ms) = worst_import {
            ui.monospace(format!("Worst import: {worst_import_ms:.2}ms"));
        }
        let worst_sync = player_snapshot
            .diagnostics
            .worst_latencies
            .hardware_sync
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_sync_ms) = worst_sync {
            ui.monospace(format!("Worst sync: {worst_sync_ms:.2}ms"));
        }
        let worst_render_acquire = player_snapshot
            .diagnostics
            .worst_latencies
            .render_acquire
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_render_acquire_ms) = worst_render_acquire {
            ui.monospace(format!("Worst acquire: {worst_render_acquire_ms:.3}ms"));
        }
        ui.monospace(format!(
            "Pipeline pauses: {}",
            player_snapshot.diagnostics.pauses.total
        ));

        if let Some(capability_summary) = &player_snapshot.capability_summary {
            ui.separator();
            ui.heading("Capabilities");
            for line in capability_summary.lines() {
                ui.monospace(line);
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
    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use render_core::{
        ActiveColorPath, ColorPipelineSettings, HdrMetadataDiagnosticMarker,
        HdrReferenceDefaultDiagnostics, RenderDiagnostics, VideoFrameFormat,
    };

    use super::AppState;

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

    /// Проверяет, что app shell не читает внутренний present frame из player pipeline.
    #[test]
    fn app_egui_does_not_access_pipeline_present_video_frame_directly() {
        let forbidden_member = concat!("pipeline", ".", "present_video_frame");

        assert!(!include_str!("state.rs").contains(forbidden_member));
        assert!(!include_str!("main.rs").contains(forbidden_member));
    }
}
