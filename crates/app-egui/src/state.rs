/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

    /// Последняя renderer-neutral диагностика без GPU handles.
    render_diagnostics: RenderDiagnostics,

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
            render_diagnostics: RenderDiagnostics::default(),
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
        }
    }

    /// Инкрементирует счётчик кадров shell.
    #[inline]
    pub fn next_frame(&mut self) {
        self.frame_index = self.frame_index.saturating_add(1);
    }

    /// Возвращает elapsed time в секундах с момента запуска.
    #[inline]
    pub fn elapsed_seconds(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Обновляет renderer diagnostics, которые UI покажет в telemetry panel.
    pub fn set_render_diagnostics(&mut self, render_diagnostics: RenderDiagnostics) {
        self.render_diagnostics = render_diagnostics;
    }

    /// Возвращает read-only snapshot из `player-core` для UI и renderer diagnostics.
    #[must_use]
    pub fn player_snapshot(&mut self) -> PlayerSnapshot {
        let player_snapshot = self
            .player_worker
            .latest_snapshot(self.frame_counters_snapshot());
        self.publish_desktop_snapshot(&player_snapshot);
        player_snapshot
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
        self.current_local_file = Some(path.to_path_buf());
        if let Err(error) = self.player_worker.load_file(path, autoplay) {
            warn!(error = %error, "Не удалось отправить команду открытия файла в worker");
        }
    }

    /// Загружает YouTube demuxer без долговременного database/cache слоя.
    pub fn load_youtube_demuxer(
        &mut self,
        label: String,
        demuxer: Box<dyn webm_demux::Demuxer + Send>,
    ) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame();
        self.current_local_file = None;
        if let Err(error) = self.player_worker.load_demuxer(label, demuxer, autoplay) {
            warn!(error = %error, "Не удалось отправить YouTube demuxer в worker");
        }
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
        }
    }

    /// Передаёт capability report из shell/backend layer в playback worker.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        if let Err(error) = self.player_worker.set_system_capabilities(capabilities) {
            warn!(error = %error, "Не удалось отправить capability report в worker");
        }
    }

    /// Пытается получить текущий video frame для renderer-а.
    #[must_use]
    pub fn try_acquire_present_frame(&mut self) -> Option<PlayerPresentFrame> {
        let player_snapshot = self.player_snapshot();
        self.drop_stale_cached_present_frame(&player_snapshot);

        if let Some(mut present_frame) = self.player_worker.try_acquire_present_frame() {
            present_frame.stale = present_frame
                .stale_for_generation(player_snapshot.render_generation)
                || player_snapshot.timeline.stale_frame;
            self.cached_present_source_label = player_snapshot.source_label.clone();
            self.cached_present_frame = Some(present_frame.clone());
            return Some(present_frame);
        }

        self.cached_present_frame
            .clone()
            .map(|mut cached_present_frame| {
                cached_present_frame.stale = cached_present_frame
                    .stale_for_generation(player_snapshot.render_generation)
                    || player_snapshot.timeline.stale_frame;
                cached_present_frame
            })
    }

    /// Сбрасывает cached frame, когда он уже не принадлежит текущему media/render поколению.
    fn drop_stale_cached_present_frame(&mut self, player_snapshot: &PlayerSnapshot) {
        if self.cached_present_frame.is_none() {
            return;
        }

        if player_snapshot.current_video_frame.is_none() {
            self.cached_present_frame = None;
            self.cached_present_source_label = None;
            return;
        }

        if self.cached_present_source_label != player_snapshot.source_label {
            self.cached_present_frame = None;
            self.cached_present_source_label = None;
            return;
        }

        let Some(cached_present_frame) = &self.cached_present_frame else {
            return;
        };
        if cached_present_frame.render_generation != player_snapshot.render_generation {
            self.clear_cached_present_frame();
        }
    }

    /// Освобождает cached present frame и отправляет drop-ack worker-у через lease guard.
    fn clear_cached_present_frame(&mut self) {
        self.cached_present_frame = None;
        self.cached_present_source_label = None;
    }

    /// Передаёт typed render bridge error в worker-owned player session.
    pub fn report_render_error(&self, error: PlayerRenderError) {
        if let Err(send_error) = self.player_worker.report_render_error(error) {
            warn!(error = %send_error, "Не удалось отправить typed render error в worker");
        }
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
        let render_diagnostics = self.render_diagnostics.clone();
        let selected_skin = skin::skin_from_config(&self.app_config.ui.skin).unwrap_or_else(|| {
            warn!(
                skin = %self.app_config.ui.skin,
                "Config validation должна была отклонить неизвестный UI skin; используем minimal"
            );
            skin::MinimalSkin::default()
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
                    &player_snapshot,
                    &telemetry,
                    &render_diagnostics,
                    &timeline_ui_state,
                    &backend_name,
                    start_time,
                    frame_duration_estimate_ms,
                );
            }
            Self::render_center_overlay(
                ui,
                is_playing,
                error_message,
                &selected_skin,
                animation_state,
            );
        });

        self.timeline_ui_state = timeline_ui_state;
        self.handle_control_actions(window, control_actions);

        full_output
    }

    /// Завершает активный timeline scrub из shell event path.
    pub fn cancel_active_timeline_scrub(&mut self) -> bool {
        if !self.timeline_ui_state.has_active_drag() {
            return false;
        }

        self.timeline_ui_state.clear_transient_drag();
        self.send_timeline_action(TimelineAction::EndScrubCommitLatest);
        true
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
    fn send_timeline_action(&self, action: TimelineAction) {
        let command = match action {
            TimelineAction::BeginScrub => PlayerCommand::BeginScrub,
            TimelineAction::UpdateScrub(position) => {
                PlayerCommand::UpdateScrub(SeekRequest::absolute(position))
            }
            TimelineAction::EndScrubCommitLatest => PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            },
        };

        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось отправить timeline command");
        }
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
    fn render_telemetry_panel(
        ui: &mut egui::Ui,
        player_snapshot: &PlayerSnapshot,
        telemetry: &Telemetry,
        render_diagnostics: &RenderDiagnostics,
        timeline_ui_state: &TimelineUiState,
        backend_name: &str,
        start_time: std::time::Instant,
        frame_duration_estimate_ms: f64,
    ) {
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
            "Repeated: {}",
            player_snapshot.frame_counters.repeated
        ));
        ui.monospace(format!("Frame dur: {:.2}ms", frame_duration_estimate_ms));
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
        }

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
