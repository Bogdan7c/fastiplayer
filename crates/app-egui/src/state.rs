/// UI-состояние приложения.
///
/// После Phase 3 этот модуль больше не владеет media pipeline. Demuxer, audio/video
/// decoder state, очереди и playback errors находятся в `player_core::PlayerSession`.
/// `AppState` оставляет у себя только egui/winit state и shell-данные.
use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use capability_core::SystemCapabilities;
use media_core::TrackKind;
use player_core::{
    FrameCounters, PlaybackState, PlayerCommand, PlayerSession, PlayerSnapshot, PlayerTickConfig,
};
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

    /// Player session владеет состоянием воспроизведения и media pipeline.
    pub player_session: PlayerSession,

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

    /// Runtime-лимиты playback tick, собранные из config.
    tick_config: PlayerTickConfig,

    /// Версия приложения для отображения в UI.
    pub app_version: &'static str,
}

impl AppState {
    /// Создаёт новое состояние приложения и пустую player session.
    #[instrument(skip(window, telemetry, app_config, startup_error))]
    pub fn new(
        window: &Window,
        telemetry: Arc<Telemetry>,
        app_config: AppConfig,
        startup_error: Option<String>,
    ) -> Self {
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
        let tick_config = PlayerTickConfig::from(&app_config);
        let mut player_session = PlayerSession::new();
        if let Err(error) = player_session
            .dispatch_command(PlayerCommand::SetVolume(app_config.audio.volume as f32))
        {
            warn!(error = %error, "Не удалось применить начальную громкость из config");
        }

        Self {
            egui_ctx,
            egui_winit_state,
            player_session,
            frame_index: 0,
            start_time: std::time::Instant::now(),
            telemetry,
            app_config,
            startup_error,
            tick_config,
            app_version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Переключает состояние воспроизведения через player session.
    pub fn toggle_playback(&mut self) {
        if let Err(error) = self
            .player_session
            .dispatch_command(PlayerCommand::TogglePlayback)
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

    /// Возвращает runtime config для одного playback tick.
    #[must_use]
    pub const fn tick_config(&self) -> PlayerTickConfig {
        self.tick_config
    }

    /// Возвращает read-only snapshot из `player-core` для UI и renderer diagnostics.
    #[must_use]
    pub fn player_snapshot(&self) -> PlayerSnapshot {
        self.player_session
            .snapshot_with_frame_counters(self.frame_counters_snapshot())
    }

    /// Загружает локальный файл через player session.
    pub fn load_file(&mut self, path: &Path) {
        self.player_session.load_file(path);
        self.apply_opened_media_playback_policy();
    }

    /// Загружает уже открытый demuxer через player session.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn webm_demux::Demuxer>) {
        self.player_session.load_demuxer(label, demuxer);
        self.apply_opened_media_playback_policy();
    }

    /// Инициализирует video pipeline в player session.
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        self.player_session
            .init_video_pipeline(instance, adapter, device, queue);
    }

    /// Передаёт capability report из shell/backend layer в player-core.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        self.player_session.set_system_capabilities(capabilities);
    }

    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure применяются
    /// командами к `PlayerSession`.
    #[instrument(skip(self, window))]
    pub fn render_ui(&mut self, window: &Window, egui_input: egui::RawInput) -> egui::FullOutput {
        let is_playing = self.player_session.playback_state() == PlaybackState::Playing;

        if is_playing {
            self.next_frame();
        }

        let player_snapshot = self.player_snapshot();
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
        let frame_duration_estimate_ms = self
            .player_session
            .pipeline
            .video_frame_duration_estimate
            .as_secs_f64()
            * 1000.0;
        let toggle_playback_clicked = Cell::new(false);
        let open_file_clicked = Cell::new(false);
        let volume_change_request = Cell::new(None::<f32>);
        let position_change_request = Cell::new(None::<f64>);
        let player_error_message = player_snapshot
            .last_error
            .as_ref()
            .map(std::string::ToString::to_string);
        let error_message = player_error_message
            .as_deref()
            .or(self.startup_error.as_deref());

        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            let top_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            egui::Panel::top("top_bar")
                .frame(top_frame)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("YouTube Player");
                        ui.separator();

                        let backend_color = match backend_name.as_str() {
                            "VA-API VP9" | "Vulkan VP9" => egui::Color32::GREEN,
                            "Synthetic (test)" => egui::Color32::YELLOW,
                            _ => egui::Color32::GRAY,
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
                                position_change_request.set(Some(slider_position as f64));
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
                .player_session
                .dispatch_command(PlayerCommand::SetVolume(requested_volume))
        {
            warn!(error = %error, "Некорректное значение громкости из UI");
        }

        if let Some(requested_position) = position_change_request.get() {
            self.player_session
                .update_current_position(duration_from_seconds_lossy(requested_position));
        }

        full_output
    }

    /// Обрабатывает горячие клавиши shell и отправляет команды в session.
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
                    .player_session
                    .dispatch_command(PlayerCommand::SetVolume(next_volume))
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

    /// Применяет config-политику автозапуска после успешного открытия media.
    fn apply_opened_media_playback_policy(&mut self) {
        if self.app_config.player.start_paused {
            return;
        }

        if !self.player_session.has_loaded_media_pipeline() {
            return;
        }

        if let Err(error) = self.player_session.dispatch_command(PlayerCommand::Play) {
            warn!(error = %error, "Не удалось запустить playback после открытия media");
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
        frame_duration_estimate_ms: f64,
    ) {
        if player_snapshot.source_label.is_none() {
            ui.monospace("No file loaded");
            return;
        }

        for track in &player_snapshot.tracks {
            match track.kind {
                TrackKind::Video => ui.monospace(format!("Video: {}", track.codec_id)),
                TrackKind::Audio => ui.monospace(format!("Audio: {}", track.codec_id)),
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

/// Безопасно создаёт `Duration` из UI seconds.
fn duration_from_seconds_lossy(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}
