/// UI-состояние приложения.
///
/// После worker-сессии этот модуль больше не владеет media pipeline. Demuxer,
/// audio/video decoder state, очереди и playback errors находятся в playback worker.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use capability_core::SystemCapabilities;
use media_core::TrackKind;
use player_core::{
    FrameCounters, PlaybackState, PlayerCommand, PlayerPresentFrame, PlayerSnapshot, PlayerWorker,
    PlayerWorkerConfig, PlayerWorkerEvent, ScrubCommitPolicy, SeekRequest,
};
use render_core::RenderDiagnostics;
use rustiplayer_config::AppConfig;
use tracing::{instrument, warn};
use winit::window::Window;

use crate::telemetry::Telemetry;

/// Состояние приложения без владения playback pipeline.
pub struct AppState {
    /// Egui context — корневой объект egui для создания UI.
    pub egui_ctx: egui::Context,

    /// Egui_winit state — обработка ввода от winit.
    pub egui_winit_state: egui_winit::State,

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
        let worker_config =
            PlayerWorkerConfig::new(player_core::PlayerTickConfig::from(&app_config));
        let player_worker = PlayerWorker::spawn(worker_config)?;
        if let Err(error) =
            player_worker.try_send_command(PlayerCommand::SetVolume(app_config.audio.volume as f32))
        {
            warn!(error = %error, "Не удалось применить начальную громкость из config");
        }

        Ok(Self {
            egui_ctx,
            egui_winit_state,
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
        self.player_worker
            .latest_snapshot(self.frame_counters_snapshot())
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

    /// Загружает уже открытый demuxer через playback worker.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn webm_demux::Demuxer + Send>) {
        let autoplay = !self.app_config.player.start_paused;
        self.clear_cached_present_frame();
        self.current_local_file = None;
        if let Err(error) = self.player_worker.load_demuxer(label, demuxer, autoplay) {
            warn!(error = %error, "Не удалось отправить streaming demuxer в worker");
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

        if let Some(present_frame) = self.player_worker.try_acquire_present_frame() {
            self.cached_present_source_label = player_snapshot.source_label.clone();
            self.cached_present_frame = Some(present_frame.clone());
            return Some(present_frame);
        }

        self.cached_present_frame.clone()
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

    /// Передаёт fatal render error в worker-owned player session.
    pub fn mark_fatal_error(&self, error: player_core::PlayerError) {
        if let Err(send_error) = self.player_worker.mark_fatal_error(error) {
            warn!(error = %send_error, "Не удалось отправить render error в worker");
        }
    }

    /// Забирает worker events для shell telemetry.
    #[must_use]
    pub fn drain_worker_events(&self) -> Vec<PlayerWorkerEvent> {
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

        let mut volume_slider_value = player_snapshot.volume;
        let position_seconds = player_snapshot.current_position.as_secs_f64();
        let duration_seconds = player_snapshot
            .duration
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0);
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
        let toggle_playback_clicked = Cell::new(false);
        let open_file_clicked = Cell::new(false);
        let volume_change_request = Cell::new(None::<f32>);
        let begin_scrub_request = Cell::new(false);
        let update_scrub_request = Cell::new(None::<f64>);
        let end_scrub_request = Cell::new(false);
        let seek_request = Cell::new(None::<f64>);
        let player_error_message = player_snapshot
            .last_error
            .as_ref()
            .map(std::string::ToString::to_string);
        let error_message = player_error_message
            .as_deref()
            .or(self.startup_error.as_deref());
        let render_diagnostics = self.render_diagnostics.clone();

        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            let top_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            egui::Panel::top("top_bar")
                .frame(top_frame)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("YouTube Player");
                        ui.separator();

                        let backend_color = if backend_name == "Synthetic (test)" {
                            egui::Color32::YELLOW
                        } else {
                            egui::Color32::GREEN
                        };
                        ui.colored_label(backend_color, format!("Backend: {backend_name}"));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.monospace(format!("v{app_version}"));
                        });
                    });
                });

            let bottom_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            egui::Panel::bottom("controls")
                .frame(bottom_frame)
                .show_inside(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(4.0);

                        if duration_seconds > 0.0 {
                            let mut slider_position = position_seconds as f32;
                            let timeline_response = ui.add(
                                egui::Slider::new(
                                    &mut slider_position,
                                    0.0..=duration_seconds as f32,
                                )
                                .show_value(false)
                                .trailing_fill(true)
                                .custom_formatter(|secs, _| Self::format_time(secs as f64)),
                            );
                            if timeline_response.changed() {
                                if timeline_response.drag_started() {
                                    begin_scrub_request.set(true);
                                }

                                if timeline_response.dragged() {
                                    update_scrub_request.set(Some(slider_position as f64));
                                } else {
                                    seek_request.set(Some(slider_position as f64));
                                }
                            }

                            if timeline_response.drag_stopped() {
                                end_scrub_request.set(true);
                            }
                        } else {
                            ui.monospace(Self::format_time(position_seconds));
                        }

                        ui.horizontal(|ui| {
                            let play_text = if is_playing { "Pause" } else { "Play" };
                            if ui.button(play_text).clicked() {
                                toggle_playback_clicked.set(true);
                            }

                            if ui.button("Open File").clicked() {
                                open_file_clicked.set(true);
                            }

                            ui.separator();
                            ui.label("Volume:");
                            let volume_response = ui.add(
                                egui::Slider::new(&mut volume_slider_value, 0.0..=1.0)
                                    .show_value(false),
                            );
                            if volume_response.changed() {
                                volume_change_request.set(Some(volume_slider_value));
                            }
                            ui.monospace(format!("{:.0}%", volume_slider_value * 100.0));

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Fullscreen").clicked() {
                                        let is_fullscreen = window.fullscreen().is_some();
                                        if is_fullscreen {
                                            window.set_fullscreen(None);
                                        } else if let Some(monitor) = window.current_monitor() {
                                            window.set_fullscreen(Some(
                                                winit::window::Fullscreen::Borderless(Some(
                                                    monitor,
                                                )),
                                            ));
                                        }
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                    });
                });

            if self.app_config.ui.show_telemetry {
                Self::render_telemetry_panel(
                    ui,
                    &player_snapshot,
                    &telemetry,
                    &render_diagnostics,
                    &backend_name,
                    start_time,
                    frame_duration_estimate_ms,
                );
            }
            Self::render_center_overlay(ui, is_playing, error_message);
        });

        if toggle_playback_clicked.get() {
            self.toggle_playback();
        }

        if open_file_clicked.get() {
            self.open_file();
        }

        if let Some(requested_volume) = volume_change_request.get()
            && let Err(error) = self
                .player_worker
                .try_send_command(PlayerCommand::SetVolume(requested_volume))
        {
            warn!(error = %error, "Некорректное значение громкости из UI");
        }

        if begin_scrub_request.get()
            && let Err(error) = self
                .player_worker
                .try_send_command(PlayerCommand::BeginScrub)
        {
            warn!(error = %error, "Не удалось начать scrub");
        }

        if let Some(requested_position) = update_scrub_request.get() {
            let request = SeekRequest::accurate(duration_from_seconds_lossy(requested_position));
            if let Err(error) = self
                .player_worker
                .try_send_command(PlayerCommand::UpdateScrub(request))
            {
                warn!(error = %error, "Не удалось обновить scrub target");
            }
        }

        if let Some(requested_position) = seek_request.get() {
            let request = SeekRequest::accurate(duration_from_seconds_lossy(requested_position));
            if let Err(error) = self
                .player_worker
                .try_send_command(PlayerCommand::Seek(request))
            {
                warn!(error = %error, "Не удалось отправить seek request");
            }
        }

        if end_scrub_request.get()
            && let Err(error) = self
                .player_worker
                .try_send_command(PlayerCommand::EndScrub {
                    policy: ScrubCommitPolicy::CommitLatest,
                })
        {
            warn!(error = %error, "Не удалось завершить scrub");
        }

        full_output
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
                let is_fullscreen = window.fullscreen().is_some();
                if is_fullscreen {
                    window.set_fullscreen(None);
                } else if let Some(monitor) = window.current_monitor() {
                    window
                        .set_fullscreen(Some(winit::window::Fullscreen::Borderless(Some(monitor))));
                }
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
                Self::format_time(duration.as_secs_f64())
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

        let pts_us = telemetry.last_pts_us();
        let pts_secs = pts_us as f64 / 1_000_000.0;
        ui.monospace(format!("Last PTS: {}", Self::format_time(pts_secs)));

        ui.separator();
        ui.monospace(format!(
            "Audio clock: {}",
            Self::format_time(player_snapshot.current_position.as_secs_f64())
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
    fn render_center_overlay(ui: &mut egui::Ui, is_playing: bool, error_message: Option<&str>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
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

    /// Форматирует время в строку MM:SS.
    fn format_time(seconds: f64) -> String {
        let total_secs = seconds.max(0.0) as u64;
        let minutes = total_secs / 60;
        let secs = total_secs % 60;
        format!("{minutes:02}:{secs:02}")
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
}

/// Безопасно создаёт `Duration` из UI seconds.
fn duration_from_seconds_lossy(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}
