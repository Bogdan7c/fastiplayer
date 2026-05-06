/// Состояние приложения: egui UI, состояние плеера, синтетическое видео.
///
/// Этот модуль владеет:
/// - egui Context и egui_winit State для обработки ввода
/// - состоянием плеера (playing/paused, громкость, позиция)
/// - счётчиком кадров синтетического видео
/// - ссылками на телеметрию
///
/// Почему состояние отделено от рендеринга:
/// - egui — immediate mode, состояние нужно каждый кадр
/// - рендеринг может быть асинхронным (wgpu)
/// - разделение упрощает тестирование UI без GPU
use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, instrument, warn};
use video_core::AvSync;
use webm_demux::{Demuxer, TrackKind};
use winit::window::Window;

use crate::telemetry::Telemetry;

/// Состояние воспроизведения плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    /// Воспроизведение暂停ено.
    Paused,

    /// Воспроизведение активно.
    Playing,
}

/// Состояние приложения.
///
/// Владеет всеми данными, которые нужны для обновления UI
/// и управления плеером. Не содержит GPU-ресурсов.
pub struct AppState {
    /// Egui context — корневой объект egui для создания UI.
    pub egui_ctx: egui::Context,

    /// Egui_winit state — обработка ввода от winit (клавиатура, мышь).
    pub egui_winit_state: egui_winit::State,

    /// Текущее состояние плеера (playing/paused).
    pub player_state: PlayerState,

    /// Флаг дорендера хвоста после EOF.
    ///
    /// EOF останавливает чтение demuxer, но decoder/audio ещё могут иметь buffered data.
    /// Этот флаг отличает такой drain от пользовательской паузы.
    pub draining_after_eof: bool,

    /// Громкость от 0.0 до 1.0.
    pub volume: f32,

    /// Текущая позиция воспроизведения в секундах (для timeline).
    pub current_position: f64,

    /// Общая длительность контента в секундах.
    pub duration: f64,

    /// Счётчик кадров синтетического видео.
    /// Используется для анимации и расчёта elapsed time.
    pub frame_index: u64,

    /// Время запуска приложения для расчёта elapsed time.
    pub start_time: std::time::Instant,

    /// Телеметрия — общие счётчики производительности.
    pub telemetry: Arc<Telemetry>,

    /// Индикатор текущего бэкенда видео.
    /// На Этапе 1 всегда "Synthetic (test)".
    pub video_backend: &'static str,

    /// Версия приложения для отображения в UI.
    pub app_version: &'static str,

    /// Demuxer для чтения WebM файлов.
    pub demuxer: Option<Box<dyn Demuxer>>,

    /// Путь к загруженному файлу.
    pub file_path: Option<PathBuf>,

    /// Сообщение об ошибке для отображения в UI.
    pub error_message: Option<String>,

    /// Audio decoder для Opus трека.
    pub audio_decoder: Option<audio::OpusDecoder>,

    /// Audio output — cpal stream + ring buffer.
    pub audio_output: Option<audio::AudioOutput>,

    /// Track ID audio трека (для фильтрации packets).
    pub audio_track_id: Option<u32>,

    /// Очередь сырых audio packets для throttle.
    /// Предотвращает переполнение ring buffer при burst decode.
    pub pending_audio_packets: VecDeque<(u32, Vec<u8>)>,

    /// Video decoder thread — VA-API VP9 decode в отдельном потоке.
    pub video_decoder_thread: Option<video_vaapi::VideoDecodeThread>,

    /// A/V sync logic для синхронизации видео и аудио.
    pub av_sync: AvSync,

    /// Очередь декодированных видеокадров перед presentation.
    pub video_frame_queue: VecDeque<video_core::DecodedFrame>,

    /// Текущий кадр для отображения (если синхронизация разрешила показ).
    pub present_video_frame: Option<video_core::DecodedFrame>,

    /// Track ID video трека.
    pub video_track_id: Option<u32>,

    /// Очередь сырых video packets для decode.
    pub pending_video_packets: VecDeque<(u32, std::time::Duration, Vec<u8>, bool)>,

    /// Audio clock для A/V sync.
    pub audio_clock: Option<std::sync::Arc<audio::clock::AudioClock>>,

    /// Последнее значение audio clock для обнаружения stalled audio.
    /// Используется вместе с [`Self::last_audio_clock_change_at`], чтобы отличать реальный
    /// underrun/EOF от обычной паузы между двумя CPAL callbacks.
    pub last_audio_clock: std::time::Duration,

    /// Момент последнего изменения audio clock.
    ///
    /// Render loop может работать чаще, чем CPAL callback. Поэтому нельзя считать audio
    /// stalled по одному одинаковому значению clock между двумя кадрами рендера.
    pub last_audio_clock_change_at: std::time::Instant,
}

impl AppState {
    /// Создаёт новое состояние приложения.
    ///
    /// Инициализирует egui context, egui_winit state,
    /// и устанавливает начальные значения плеера.
    #[instrument(skip(window, telemetry))]
    pub fn new(window: &Window, telemetry: Arc<Telemetry>) -> Self {
        let egui_ctx = egui::Context::default();

        // Настраиваем egui для тёмной темы (лучше для видеоплеера)
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

        Self {
            egui_ctx,
            egui_winit_state,
            player_state: PlayerState::Paused,
            draining_after_eof: false,
            volume: 0.8,
            current_position: 0.0,
            duration: 0.0,
            frame_index: 0,
            start_time: std::time::Instant::now(),
            telemetry,
            video_backend: "Synthetic (test)",
            app_version: env!("CARGO_PKG_VERSION"),
            demuxer: None,
            file_path: None,
            error_message: None,
            audio_decoder: None,
            audio_output: None,
            audio_track_id: None,
            pending_audio_packets: VecDeque::new(),
            video_decoder_thread: None,
            av_sync: AvSync::default(),
            video_frame_queue: VecDeque::new(),
            present_video_frame: None,
            video_track_id: None,
            pending_video_packets: VecDeque::new(),
            audio_clock: None,
            last_audio_clock: std::time::Duration::ZERO,
            last_audio_clock_change_at: std::time::Instant::now(),
        }
    }

    /// Переключает состояние воспроизведения (play/pause toggle).
    #[inline]
    pub fn toggle_playback(&mut self) {
        self.player_state = match self.player_state {
            PlayerState::Playing => PlayerState::Paused,
            PlayerState::Paused => PlayerState::Playing,
        };

        // Ручное переключение playback отменяет специальный EOF-drain режим.
        self.draining_after_eof = false;

        // При pause очищаем только будущие packets/frames, но оставляем текущий кадр на экране.
        if self.player_state == PlayerState::Paused {
            // Сырые pending packets сохраняем для resume.
            // Их уже прочитал demuxer, поэтому очистка сдвинула бы demuxer вперёд
            // относительно audio clock и могла бы навсегда заблокировать video decode-ahead.
            self.clear_queued_video_frames();
        }

        // Запускаем/останавливаем audio stream
        if let Some(ref mut output) = self.audio_output {
            if self.player_state == PlayerState::Playing {
                if let Err(e) = output.play() {
                    warn!(error = %e, "Не удалось запустить audio");
                }
                self.last_audio_clock = self.audio_clock_now();
                self.last_audio_clock_change_at = std::time::Instant::now();
            } else {
                if let Err(e) = output.pause() {
                    warn!(error = %e, "Не удалось остановить audio");
                }
            }
        }
    }

    /// Обновляет позицию воспроизведения на основе прошедшего времени.
    ///
    /// Вызывается каждый кадр, когда плеер в состоянии Playing.
    /// На Этапе 1 симулирует прогресс со скоростью 1x.
    #[inline]
    pub fn update_position(&mut self, delta_seconds: f64) {
        if self.player_state == PlayerState::Playing {
            self.current_position += delta_seconds;
        }
    }

    /// Инкрементирует счётчик кадров.
    #[inline]
    pub fn next_frame(&mut self) {
        self.frame_index += 1;
    }

    /// Возвращает elapsed time в секундах с момента запуска.
    #[inline]
    pub fn elapsed_seconds(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Форматирует время в строку MM:SS.
    fn format_time(seconds: f64) -> String {
        let total_secs = seconds as u64;
        let minutes = total_secs / 60;
        let secs = total_secs % 60;
        format!("{:02}:{:02}", minutes, secs)
    }

    /// Рендерит egui UI поверх видео.
    ///
    /// Создаёт:
    /// - верхнюю панель с названием и версией
    /// - нижнюю панель с элементами управления плеером
    /// - оверлей телеметрии (FPS, frame time, dropped frames)
    ///
    /// Возвращает FullOutput egui для последующего рендеринга через egui_wgpu.
    ///
    /// Почему Cell<bool> для toggle_playback_clicked:
    /// egui::Context::run_ui требует closure, который захватывает переменные.
    /// Но внутри closure мы не можем вызывать &mut self методы.
    /// Cell позволяет изменять значение из immutable closure.
    #[instrument(skip(self, window))]
    pub fn render_ui(&mut self, window: &Window, egui_input: egui::RawInput) -> egui::FullOutput {
        let is_playing = self.player_state == PlayerState::Playing;
        let volume_before_ui = self.volume;

        // Обновляем позицию если играем
        if is_playing {
            self.update_position(1.0 / 60.0);
            self.next_frame();
        }

        // Cell позволяет менять значение из immutable closure
        let toggle_playback_clicked = Cell::new(false);
        let open_file_clicked = Cell::new(false);

        // Извлекаем audio данные ДО mutable borrows
        let audio_clock_secs = self.audio_clock_secs();
        let audio_buffer_ms = self.audio_buffer_level_ms();

        // Извлекаем поля для closure
        let video_backend = self.video_backend;
        let app_version = self.app_version;
        let duration = self.duration;
        let current_position = &mut self.current_position;
        let volume = &mut self.volume;
        let telemetry = &self.telemetry;
        let start_time = &self.start_time;
        let error_message = &self.error_message;

        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            // === Верхняя панель: название и бэкенд ===
            // Полупрозрачный фон, чтобы видео было видно под панелью
            let top_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            egui::Panel::top("top_bar")
                .frame(top_frame)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("YouTube Player");
                        ui.separator();

                        let backend_color = match video_backend {
                            "VA-API VP9" | "Vulkan VP9" => egui::Color32::GREEN,
                            "Synthetic (test)" => egui::Color32::YELLOW,
                            _ => egui::Color32::GRAY,
                        };
                        ui.colored_label(backend_color, format!("Backend: {}", video_backend));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.monospace(format!("v{}", app_version));
                        });
                    });
                });

            // === Нижняя панель: управление плеером ===
            let bottom_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            egui::Panel::bottom("controls")
                .frame(bottom_frame)
                .show_inside(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(4.0);

                        // Timeline
                        if duration > 0.0 {
                            let mut pos = *current_position as f32;
                            ui.add(
                                egui::Slider::new(&mut pos, 0.0..=duration as f32)
                                    .show_value(false)
                                    .trailing_fill(true)
                                    .custom_formatter(|secs, _| Self::format_time(secs as f64)),
                            );
                            *current_position = pos as f64;
                        } else {
                            ui.monospace(Self::format_time(*current_position));
                        }

                        // Кнопки управления
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
                            ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false));
                            ui.monospace(format!("{:.0}%", *volume * 100.0));

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

            // === Оверлей телеметрии ===
            let telemetry_frame =
                egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160));
            egui::Panel::right("telemetry")
                .resizable(false)
                .exact_size(200.0)
                .frame(telemetry_frame)
                .show_inside(ui, |ui| {
                    ui.heading("Telemetry");
                    ui.separator();

                    let fps = telemetry.current_fps();
                    let frame_time = telemetry.last_frame_time_ms();
                    let presented = telemetry.presented_frames();
                    let dropped = telemetry.dropped_frames();
                    let drop_rate = telemetry.drop_rate_percent();

                    ui.monospace(format!("FPS: {}", fps));
                    ui.monospace(format!("Frame time: {:.2} ms", frame_time as f64));
                    ui.separator();
                    ui.monospace(format!("Presented: {}", presented));
                    ui.monospace(format!("Dropped: {}", dropped));
                    ui.monospace(format!("Drop rate: {:.2}%", drop_rate));
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
                    ui.monospace(format!("Backend: {}", video_backend));
                    ui.monospace(format!(
                        "Elapsed: {:.1}s",
                        start_time.elapsed().as_secs_f64()
                    ));

                    // === Media Info ===
                    ui.separator();
                    ui.heading("Media Info");

                    if let Some(ref demuxer) = self.demuxer {
                        let tracks = demuxer.tracks();
                        let video_tracks: Vec<_> = tracks
                            .iter()
                            .filter(|t| t.kind == webm_demux::TrackKind::Video)
                            .collect();
                        let audio_tracks: Vec<_> = tracks
                            .iter()
                            .filter(|t| t.kind == webm_demux::TrackKind::Audio)
                            .collect();

                        for t in &video_tracks {
                            ui.monospace(format!("Video: {}", t.codec_id));
                        }
                        for t in &audio_tracks {
                            ui.monospace(format!("Audio: {}", t.codec_id));
                        }

                        if let Some(dur) = demuxer.duration() {
                            ui.monospace(format!(
                                "Duration: {}",
                                Self::format_time(dur.as_secs_f64())
                            ));
                        }

                        if let Some(path) = &self.file_path {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy())
                                .unwrap_or_default();
                            ui.monospace(format!("File: {}", name));
                        }

                        // Packet stats
                        ui.separator();
                        let total = telemetry.packets_read();
                        let video_pkts = telemetry.video_packets();
                        let audio_pkts = telemetry.audio_packets();
                        ui.monospace(format!("Packets: {}", total));
                        ui.monospace(format!("  Video: {}", video_pkts));
                        ui.monospace(format!("  Audio: {}", audio_pkts));

                        let pts_us = telemetry.last_pts_us();
                        let pts_secs = pts_us as f64 / 1_000_000.0;
                        ui.monospace(format!("Last PTS: {}", Self::format_time(pts_secs)));

                        // Audio clock и buffer level
                        if let Some(audio_secs) = audio_clock_secs {
                            ui.separator();
                            ui.monospace(format!("Audio clock: {}", Self::format_time(audio_secs)));
                        }
                        if let Some(buffer_ms) = audio_buffer_ms {
                            let buf_color = if buffer_ms > 10.0 {
                                egui::Color32::GREEN
                            } else if buffer_ms > 0.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::RED
                            };
                            ui.colored_label(buf_color, format!("Audio buf: {:.1}ms", buffer_ms));
                        }

                        // Video telemetry
                        if let Some(ref thread) = self.video_decoder_thread {
                            ui.separator();
                            ui.heading("Video");
                            ui.monospace(format!("Backend: {}", thread.backend_name()));
                            ui.monospace(format!("Decoded: {}", telemetry.video_frames_decoded()));
                            ui.monospace(format!(
                                "Presented: {}",
                                telemetry.video_frames_presented()
                            ));
                            ui.monospace(format!("Dropped: {}", telemetry.video_frames_dropped()));
                            ui.monospace(format!("Queue: {}", self.video_frame_queue.len()));
                            if let Some(stats) = thread.texture_pool_stats() {
                                ui.monospace(format!(
                                    "Textures: {}/{}",
                                    stats.in_use, stats.capacity
                                ));
                                ui.monospace(format!("Texture slots: {}", stats.slots));
                            }
                        }
                    } else {
                        ui.monospace("No file loaded");
                    }
                });

            // === Центральная область: подсказки ===
            // Frame::NONE делает панель прозрачной, чтобы видео было видно под UI
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
                            ui.add_space(10.0);
                            ui.monospace("Hotkeys:");
                            ui.monospace("  Space — Play/Pause");
                            ui.monospace("  F — Fullscreen");
                            ui.monospace("  M — Mute");
                            ui.monospace("  Esc — Exit");
                        });
                    }
                });
        });

        // Обрабатываем play/pause клик после closure
        if toggle_playback_clicked.get() {
            self.toggle_playback();
        }

        // Обрабатываем open file клик после closure
        if open_file_clicked.get() {
            self.open_file();
        }

        if (self.volume - volume_before_ui).abs() > f32::EPSILON {
            if let Some(ref mut output) = self.audio_output {
                output.set_volume(self.volume);
            }
        }

        full_output
    }

    /// Обрабатывает горячие клавиши плеера.
    ///
    /// Вызывается после обработки egui, чтобы реагировать на клавиши
    /// только когда egui не захватил ввод (например, когда фокус не на виджете).
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
                if self.volume > 0.0 {
                    self.volume = 0.0;
                } else {
                    self.volume = 0.8;
                }
                if let Some(ref mut output) = self.audio_output {
                    output.set_volume(self.volume);
                }
                true
            }
            _ => false,
        }
    }

    /// Открывает WebM файл через file dialog.
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

    /// Загружает файл в demuxer.
    pub fn load_file(&mut self, path: &PathBuf) {
        self.reset_media_state();

        match webm_demux::SymphoniaDemuxer::from_file(path) {
            Ok(demuxer) => {
                let tracks = demuxer.tracks();
                let duration = demuxer.duration();
                info!(
                    path = %path.display(),
                    tracks = tracks.len(),
                    duration = ?duration,
                    "Файл загружен"
                );

                // Ищем audio track и инициализируем audio pipeline
                self.init_audio_pipeline(tracks);

                // Ищем video track
                let video_track = tracks
                    .iter()
                    .find(|t| t.kind == webm_demux::TrackKind::Video && t.codec_id == "V_VP9");
                if let Some(track) = video_track {
                    self.video_track_id = Some(track.id);
                } else {
                    info!("Поддерживаемый VP9 video track не найден");
                }

                self.demuxer = Some(Box::new(demuxer));
                self.file_path = Some(path.clone());
                self.error_message = None;
                self.duration = duration.map(|d| d.as_secs_f64()).unwrap_or(0.0);
            }
            Err(e) => {
                warn!(error = %e, "Не удалось открыть файл");
                self.error_message = Some(format!("Ошибка: {}", e));
            }
        }
    }

    /// Загружает уже открытый demuxer.
    ///
    /// Используется для streaming sources, где media не существует как готовый локальный файл.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn Demuxer>) {
        self.reset_media_state();

        let tracks = demuxer.tracks();
        let duration = demuxer.duration();

        info!(
            source = %label,
            tracks = tracks.len(),
            duration = ?duration,
            "Streaming demuxer загружен"
        );

        // Ищем audio track и инициализируем audio pipeline.
        self.init_audio_pipeline(tracks);

        // Ищем VP9 video track.
        let video_track = tracks
            .iter()
            .find(|track| track.kind == webm_demux::TrackKind::Video && track.codec_id == "V_VP9");
        if let Some(track) = video_track {
            self.video_track_id = Some(track.id);
        } else {
            info!("Поддерживаемый VP9 video track не найден в streaming demuxer");
        }

        self.demuxer = Some(demuxer);
        self.file_path = None;
        self.error_message = None;
        self.duration = duration.map(|value| value.as_secs_f64()).unwrap_or(0.0);
    }

    /// Полностью сбрасывает состояние текущего media-файла.
    ///
    /// Вызывается перед загрузкой нового файла, чтобы старые decoder/audio/track id
    /// не влияли на следующий playback.
    pub fn reset_media_state(&mut self) {
        self.player_state = PlayerState::Paused;
        self.draining_after_eof = false;
        self.clear_video_frames();
        if let Some(ref thread) = self.video_decoder_thread
            && let Err(error) = thread.flush()
        {
            warn!(error = %error, "Не удалось сбросить video decoder thread");
        }
        self.demuxer = None;
        self.file_path = None;
        self.audio_decoder = None;
        self.audio_output = None;
        self.audio_track_id = None;
        self.pending_audio_packets.clear();
        self.video_track_id = None;
        self.pending_video_packets.clear();
        self.audio_clock = None;
        self.current_position = 0.0;
        self.duration = 0.0;
        self.last_audio_clock = std::time::Duration::ZERO;
        self.last_audio_clock_change_at = std::time::Instant::now();
    }

    /// Очищает video frame queue и present frame, освобождая texture slots.
    pub fn clear_video_frames(&mut self) {
        if let Some(ref thread) = self.video_decoder_thread {
            for frame in self.video_frame_queue.drain(..) {
                thread.release_frame(frame.texture_handle);
            }
            if let Some(ref frame) = self.present_video_frame {
                thread.release_frame(frame.texture_handle);
            }
        }
        self.video_frame_queue.clear();
        self.present_video_frame = None;
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        if let Some(ref thread) = self.video_decoder_thread {
            for frame in self.video_frame_queue.drain(..) {
                thread.release_frame(frame.texture_handle);
            }
        }
        self.video_frame_queue.clear();
    }

    /// Инициализирует audio pipeline если есть Opus audio track.
    fn init_audio_pipeline(&mut self, tracks: &[webm_demux::TrackInfo]) {
        // Находим первый audio track с известными параметрами
        let audio_track = tracks.iter().find(|t| {
            t.kind == TrackKind::Audio && t.sample_rate.is_some() && t.channels.is_some()
        });

        let Some(track) = audio_track else {
            info!("Audio track не найден или параметры неизвестны — playback без звука");
            return;
        };

        let (Some(sample_rate), Some(channels)) = (track.sample_rate, track.channels) else {
            warn!(
                track_id = track.id,
                "Audio track выбран без sample_rate/channels"
            );
            return;
        };

        info!(
            track_id = track.id,
            codec = %track.codec_id,
            sample_rate,
            channels,
            "Инициализация audio pipeline"
        );

        // Создаём Opus decoder
        match audio::OpusDecoder::new(sample_rate, channels) {
            Ok(decoder) => {
                self.audio_decoder = Some(decoder);
            }
            Err(e) => {
                warn!(error = %e, "Не удалось создать Opus decoder");
                self.error_message = Some(format!("Audio error: {}", e));
                return;
            }
        }

        // Создаём AudioOutput (cpal stream + ring buffer)
        match audio::AudioOutput::new(sample_rate, channels) {
            Ok(mut output) => {
                output.set_volume(self.volume);
                self.audio_output = Some(output);
            }
            Err(e) => {
                warn!(error = %e, "Не удалось создать AudioOutput");
                self.error_message = Some(format!("Audio error: {}", e));
                return;
            }
        }

        self.audio_track_id = Some(track.id);

        // Сохраняем audio clock для A/V sync
        if let Some(ref output) = self.audio_output {
            self.audio_clock = Some(output.clock().clone());
        }

        info!("Audio pipeline инициализирован");
    }

    /// Обрабатывает audio packet: decode → write to AudioOutput.
    ///
    /// Вызывается из render_frame() для каждого audio packet.
    pub fn process_audio_packet(&mut self, track_id: u32, data: &[u8]) {
        // Пропускаем packets не нашего audio трека
        if self.audio_track_id != Some(track_id) {
            return;
        }

        // Декодируем Opus packet
        if let Some(ref mut decoder) = self.audio_decoder {
            match decoder.decode(data) {
                Ok(samples) if !samples.is_empty() => {
                    // Записываем decoded samples в ring buffer
                    if let Some(ref mut output) = self.audio_output {
                        output.write_samples(&samples);
                    }
                }
                Ok(_) => {
                    // Пустой packet (corrupted) — пропускаем
                }
                Err(e) => {
                    warn!(error = %e, "Ошибка декодирования audio packet");
                }
            }
        }
    }

    /// Process pending audio packets с throttle по buffer level.
    ///
    /// Проверяем fill level ring buffer и декодируем только если есть место.
    /// Это предотвращает overflow при burst read из demuxer.
    pub fn process_pending_audio_packets(&mut self) {
        // Определяем сколько audio ms уже в buffer
        let buffer_ms = self.audio_buffer_level_ms().unwrap_or(0.0);

        // Throttle: не декодируем если buffer > 200ms
        if buffer_ms > 200.0 {
            return;
        }

        // Декодируем packets пока buffer не заполнится
        while let Some((track_id, data)) = self.pending_audio_packets.pop_front() {
            // Проверяем buffer level перед каждым packet
            let buffer_ms = self.audio_buffer_level_ms().unwrap_or(0.0);
            if buffer_ms > 200.0 {
                self.pending_audio_packets.push_front((track_id, data));
                break;
            }

            // Декодируем и записываем
            self.process_audio_packet(track_id, &data);
        }
    }

    /// Возвращает audio clock time для отображения в UI.
    pub fn audio_clock_secs(&self) -> Option<f64> {
        self.audio_output
            .as_ref()
            .map(|output| output.clock().now_secs())
    }

    /// Возвращает уровень audio buffer в миллисекундах.
    pub fn audio_buffer_level_ms(&self) -> Option<f64> {
        self.audio_output
            .as_ref()
            .map(|output| output.buffer_level_ms())
    }

    /// Возвращает текущее время audio clock.
    pub fn audio_clock_now(&self) -> std::time::Duration {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.now())
            .unwrap_or(std::time::Duration::ZERO)
    }

    /// Инициализирует video pipeline (VA-API decoder).
    ///
    /// Пробует создать VA-API VP9 hardware decoder. Если VA-API недоступна
    /// (нет драйвера, не поддерживается VP9), логирует warning и продолжает
    /// работу без hardware decode (приложение покажет synthetic video).
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let device = Arc::new(device.clone());
        let queue = Arc::new(queue.clone());

        match video_vaapi::VideoDecodeThread::new(device, queue, instance.clone(), adapter.clone())
        {
            Ok(thread) => {
                self.video_backend = thread.backend_name();
                self.video_decoder_thread = Some(thread);
                info!(backend = self.video_backend, "Video decoder thread started");
            }
            Err(e) => {
                warn!(error = %e, "VA-API decoder thread unavailable, no hardware decode");
            }
        }
    }
}
