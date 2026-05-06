/// Окно приложения и главный цикл рендеринга.
///
/// Этот модуль отвечает за:
/// - создание окна через winit
/// - обработку событий окна (ресайз, закрытие, ввод)
/// - запуск egui для UI overlay
/// - поддержание 60 fps через VSync (Fifo present mode)
/// - координацию между рендерингом видео и UI
///
/// Почему winit + wgpu, а не eframe:
/// eframe скрывает детали инициализации, но нам нужен
/// прямой контроль над swapchain и render pass для zero-copy video.
/// В будущем decoded VkImage будет рендериться напрямую без CPU readback,
/// и eframe не даст нужного уровня контроля.
///
/// Архитектура event loop:
/// - winit 0.30 использует ApplicationHandler trait
/// - события: Resumed (создание окна), Suspended (уничтожение), WindowEvent
/// - RedrawRequested — основной hook для рендеринга каждого кадра
mod render;
mod state;
mod telemetry;
mod youtube;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info, instrument, trace, warn};
use winit::{
    application::ApplicationHandler, dpi::PhysicalSize, event::WindowEvent,
    event_loop::ActiveEventLoop, window::Window,
};

use crate::render::Renderer;
use crate::state::AppState;
use crate::telemetry::{Telemetry, VideoDropReason};

/// Media, которое нужно автоматически открыть после создания окна.
enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// Уже открытый streaming demuxer.
    Streaming {
        /// Описание stream для логов/UI.
        label: String,

        /// Demuxer, читающий из HTTP-backed потоков.
        demuxer: Box<dyn webm_demux::Demuxer>,
    },
}

/// Минимальная длительность без движения audio clock, после которой считаем звук stalled.
///
/// CPAL callback обычно обновляет clock пачками samples, а render loop может сработать между
/// двумя callbacks. Поэтому один одинаковый `audio_now` ещё не означает underrun.
const AUDIO_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Сколько container packets читаем за один redraw.
///
/// Это ограничивает read-ahead: decoder не должен убегать на секунды вперёд от presentation.
const MAX_DEMUX_PACKETS_PER_REDRAW: usize = 6;

/// Максимум готовых видеокадров в очереди presentation.
///
/// 8 кадров дают примерно 133ms при 60 FPS и примерно 267ms при 30 FPS.
/// Этого достаточно для сглаживания decode jitter, но очередь больше не
/// забирает почти весь zero-copy texture pool.
const MAX_VIDEO_PRESENT_QUEUE: usize = 8;

/// Минимальный запас свободных texture slots перед отправкой новых video packets.
///
/// Запас нужен для текущего present frame, кадров в decoder channel и transient
/// imported textures, которые освобождаются после `on_submitted_work_done`.
const MIN_TEXTURE_SLOTS_AVAILABLE_FOR_DECODE: usize = 2;

/// Максимум сырых video packets, которые можно держать между demuxer и decoder thread.
///
/// Этот лимит важен для pause/resume: demuxer не должен уходить далеко вперёд, пока
/// decode-ahead guard ждёт audio clock. Иначе после resume следующий video packet будет
/// слишком далеко от audio clock и видео не сможет продолжить декодирование.
const MAX_PENDING_VIDEO_PACKETS: usize = 8;

/// Максимум video packets, которые render thread отправляет decoder thread за один redraw.
///
/// Декодер может работать быстрее realtime, особенно когда кадры уже лежат в demuxer.
/// Маленький лимит не даёт command channel убежать далеко вперёд от audio clock.
const MAX_VIDEO_PACKETS_SENT_PER_REDRAW: usize = 2;

/// Максимальный допустимый decode-ahead относительно audio clock.
///
/// Если отправлять в decoder thread кадры на секунды вперёд, A/V sync будет честно ждать
/// звук, а пользователь увидит "зависший" старый кадр. 500ms оставляют рабочий буфер,
/// но не позволяют видео накопить многосекундный отрыв.
const MAX_VIDEO_DECODE_AHEAD: std::time::Duration = std::time::Duration::from_millis(500);

/// Небольшой phase lead при выборе кадра относительно audio clock.
///
/// Половина video frame компенсирует задержку render→present и не заставляет
/// scheduler перескакивать кадры из-за мелкого phase jitter.
const VIDEO_PRESENT_LEAD_FRAMES: f64 = 0.5;

/// Окно, внутри которого следующий sequential frame можно показывать без ожидания.
const VIDEO_PRESENT_WINDOW_FRAMES: f64 = 1.0;

/// Запас перед late-drop.
///
/// Даже если следующий кадр математически ближе к target, текущий кадр лучше
/// показать, если он не опоздал реально. Иначе 60 FPS легко превращаются в
/// регулярный skip на границе половины frame interval.
const VIDEO_LATE_DROP_GRACE_FRAMES: f64 = 2.0;

/// Главное приложение — реализует ApplicationHandler для winit.
///
/// Владеет:
/// - окном (Arc<Window>)
/// - рендерером (GPU + video pipeline + egui_wgpu)
/// - состоянием приложения (egui + player state)
/// - телеметрией
/// - путём к файлу для автозагрузки из CLI
struct App {
    /// Окно приложения. None до Resumed.
    window: Option<Arc<Window>>,

    /// Рендерер с GPU ресурсами. None до Resumed.
    renderer: Option<Renderer>,

    /// Состояние приложения (egui, player state). None до Resumed.
    app_state: Option<AppState>,

    /// Общая телеметрия.
    telemetry: Arc<Telemetry>,

    /// Media, переданное через CLI, для автозагрузки при старте.
    initial_media: Option<InitialMedia>,

    /// Сообщение об ошибке, которое нужно показать после создания UI.
    initial_error: Option<String>,
}

impl App {
    /// Создаёт пустое приложение.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    fn new(initial_media: Option<InitialMedia>, initial_error: Option<String>) -> Self {
        Self {
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            initial_media,
            initial_error,
        }
    }

    /// Создаёт или пересоздаёт runtime-ресурсы, завязанные на активное окно.
    ///
    /// Winit 0.30 вызывает `resumed` не только при первом старте, но и после возврата
    /// приложения из suspended-состояния. Окно при этом может уже существовать, а surface
    /// и GPU-ресурсы могли быть сброшены. Поэтому восстановление renderer/app_state
    /// отделено от создания окна.
    fn restore_runtime(&mut self, event_loop: &ActiveEventLoop, window: Arc<Window>) {
        if self.renderer.is_some() && self.app_state.is_some() {
            return;
        }

        let renderer = match Renderer::new(window.clone(), self.telemetry.clone()) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::error!("Не удалось инициализировать рендерер: {}", error);
                event_loop.exit();
                return;
            }
        };

        let mut app_state = AppState::new(&window, self.telemetry.clone());
        app_state.init_video_pipeline(
            &renderer.gpu.instance,
            &renderer.gpu.adapter,
            &renderer.gpu.device,
            &renderer.gpu.queue,
        );

        if let Some(error) = self.initial_error.take() {
            app_state.error_message = Some(error);
        }

        if let Some(initial_media) = self.initial_media.take() {
            match initial_media {
                InitialMedia::File(path) => {
                    info!(path = %path.display(), "Автозагрузка файла из CLI");
                    app_state.load_file(&path);
                }
                InitialMedia::Streaming { label, demuxer } => {
                    info!(source = %label, "Автозагрузка streaming media из CLI");
                    app_state.load_demuxer(label, demuxer);
                }
            }

            if app_state.demuxer.is_some() {
                app_state.toggle_playback();
            }
        }

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn drop_runtime(&mut self) {
        if let Some(app_state) = &self.app_state
            && let Some(path) = &app_state.file_path
        {
            self.initial_media = Some(InitialMedia::File(path.clone()));
        }
        self.app_state = None;
        self.renderer = None;
    }
}

impl ApplicationHandler for App {
    /// Вызывается при приостановке приложения (сворачивание, смена TTY).
    ///
    /// Освобождаем GPU ресурсы — surface может стать невалидным.
    #[instrument(skip(self))]
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        info!("Приостановка: освобождаем runtime-ресурсы");
        self.drop_runtime();
    }

    /// Вызывается при возобновлении работы (разворачивание, первый запуск).
    ///
    /// Здесь создаём окно, инициализируем wgpu и egui.
    #[instrument(skip(self, event_loop))]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.clone() {
            self.restore_runtime(event_loop, window);
            return;
        }

        info!("Resumed: создание окна");

        let window_attributes = Window::default_attributes()
            .with_title("YouTube Player — Stage 1 (Render Shell)")
            .with_inner_size(winit::dpi::PhysicalSize::new(1280, 720))
            .with_visible(true);

        let window = match event_loop.create_window(window_attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("Не удалось создать окно: {}", e);
                event_loop.exit();
                return;
            }
        };

        info!(
            width = window.inner_size().width,
            height = window.inner_size().height,
            scale_factor = window.scale_factor(),
            "Окно создано"
        );

        self.window = Some(window.clone());
        self.restore_runtime(event_loop, window);
    }

    /// Обработка событий окна.
    ///
    /// Основной поток событий: ввод, ресайз, закрытие, redraw.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let (Some(window), Some(renderer), Some(app_state)) =
            (&self.window, &mut self.renderer, &mut self.app_state)
        else {
            return;
        };

        // Передаём событие в egui_winit для обработки ввода
        let egui_response = app_state.egui_winit_state.on_window_event(window, &event);

        // Если egui потребил событие (например, клик по кнопке), не обрабатываем дальше
        if egui_response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                info!("Закрытие окна по запросу пользователя");
                self.drop_runtime();
                event_loop.exit();
            }

            WindowEvent::Resized(PhysicalSize { width, height }) => {
                debug!(width, height, "Изменение размера окна");
                renderer.gpu.resize(width, height);
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                debug!(scale_factor, "Изменение масштаба");
                app_state.egui_ctx.set_pixels_per_point(scale_factor as f32);
            }

            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        physical_key: winit::keyboard::PhysicalKey::Code(key_code),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } => match key_code {
                winit::keyboard::KeyCode::Escape => {
                    info!("Выход по Escape");
                    self.drop_runtime();
                    event_loop.exit();
                }
                other => {
                    app_state.handle_hotkeys(window, other, egui_response.consumed);
                }
            },

            WindowEvent::RedrawRequested => {
                render_frame(&self.telemetry, window, renderer, app_state);
            }

            _ => {}
        }

        // Запрашиваем следующий кадр для непрерывного рендеринга
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// Проверяет, можно ли отправить video packet в decoder thread без чрезмерного decode-ahead.
fn can_send_video_packet_to_decoder(app_state: &AppState, packet_pts: std::time::Duration) -> bool {
    if !has_texture_capacity_for_decode(app_state) {
        return false;
    }

    // Для файлов без audio track нет внешнего clock, поэтому video может идти самостоятельно.
    if app_state.audio_track_id.is_none() || app_state.audio_clock.is_none() {
        return true;
    }

    // Audio clock — главный источник времени для A/V sync.
    let audio_now = app_state.audio_clock_now();

    // Saturating diff защищает от underflow, когда packet слегка позади audio clock.
    let packet_lead = packet_pts.saturating_sub(audio_now);

    // Decode-ahead разрешён только в пределах небольшого буфера.
    packet_lead <= MAX_VIDEO_DECODE_AHEAD
}

/// Проверяет, есть ли запас texture slots для ещё одного decoded frame.
fn has_texture_capacity_for_decode(app_state: &AppState) -> bool {
    let Some(ref thread) = app_state.video_decoder_thread else {
        return true;
    };

    let Some(stats) = thread.texture_pool_stats() else {
        return true;
    };

    let available_slots = stats.available_slots();
    if available_slots <= MIN_TEXTURE_SLOTS_AVAILABLE_FOR_DECODE {
        trace!(
            texture_slots = stats.slots,
            texture_in_use = stats.in_use,
            texture_capacity = stats.capacity,
            available_slots,
            reserve = MIN_TEXTURE_SLOTS_AVAILABLE_FOR_DECODE,
            "Video backpressure: waiting for texture slots"
        );
        return false;
    }

    true
}

/// Проверяет, можно ли читать следующий packet из demuxer.
fn can_read_next_demux_packet(app_state: &AppState) -> bool {
    // Если texture pool почти заполнен, сначала даём presentation освободить frames.
    if !has_texture_capacity_for_decode(app_state) {
        return false;
    }

    // Если очередь сырых video packets заполнена, demuxer должен подождать decoder.
    if app_state.pending_video_packets.len() >= MAX_PENDING_VIDEO_PACKETS {
        return false;
    }

    // Если presentation queue заполнена, чтение новых packets только увеличит задержку.
    if app_state.video_frame_queue.len() >= MAX_VIDEO_PRESENT_QUEUE {
        return false;
    }

    true
}

/// Кладёт decoded frame в presentation queue, сохраняя фиксированный размер очереди.
fn enqueue_decoded_video_frame(
    app_state: &mut AppState,
    telemetry: &Telemetry,
    frame: video_core::DecodedFrame,
) {
    while app_state.video_frame_queue.len() >= MAX_VIDEO_PRESENT_QUEUE {
        let Some(stale_frame) = app_state.video_frame_queue.pop_front() else {
            break;
        };

        if let Some(ref thread) = app_state.video_decoder_thread {
            thread.release_frame(stale_frame.texture_handle);
        }

        telemetry.record_video_frame_dropped(VideoDropReason::QueueOverflow);
        tracing::debug!(
            pts_ms = stale_frame.pts.as_millis(),
            queue_limit = MAX_VIDEO_PRESENT_QUEUE,
            "Dropping oldest queued video frame before enqueue"
        );
    }

    app_state.video_frame_queue.push_back(frame);
}

/// Забирает готовые кадры из decoder thread и кладёт их в presentation queue.
fn drain_decoded_video_frames(app_state: &mut AppState, telemetry: &Telemetry) {
    let Some(ref thread) = app_state.video_decoder_thread else {
        if !app_state.pending_video_packets.is_empty() {
            tracing::warn!(
                count = app_state.pending_video_packets.len(),
                "No video decoder thread — dropping video packets"
            );
        }
        app_state.pending_video_packets.clear();
        return;
    };

    let mut decoded_frames = Vec::new();
    while let Some(frame) = thread.try_recv_frame() {
        decoded_frames.push(frame);
    }

    for frame in decoded_frames {
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            width = frame.width,
            height = frame.height,
            "Video frame decoded"
        );
        telemetry.record_video_frame_decoded();
        app_state.observe_video_frame_pts(frame.pts);

        // В Paused кадры могли доехать из decoder thread уже после клика по паузе.
        // Их нельзя показывать: pause должен заморозить текущий present frame.
        if app_state.player_state == crate::state::PlayerState::Playing
            || app_state.draining_after_eof
        {
            enqueue_decoded_video_frame(app_state, telemetry, frame);
        } else if let Some(ref thread) = app_state.video_decoder_thread {
            thread.release_frame(frame.texture_handle);
            telemetry.record_video_frame_dropped(VideoDropReason::Paused);
            tracing::debug!("Dropping decoded frame received while playback is paused");
        }
    }
}

/// Отправляет ограниченное число pending video packets в decoder thread.
fn send_pending_video_packets_to_decoder(app_state: &mut AppState) {
    let Some(ref thread) = app_state.video_decoder_thread else {
        return;
    };

    let mut sent_packets = 0usize;

    while sent_packets < MAX_VIDEO_PACKETS_SENT_PER_REDRAW {
        if app_state.video_frame_queue.len() >= MAX_VIDEO_PRESENT_QUEUE {
            break;
        }

        let Some((track_id, pts, _data, _keyframe)) = app_state.pending_video_packets.front()
        else {
            break;
        };

        if app_state.video_track_id != Some(*track_id) {
            app_state.pending_video_packets.pop_front();
            continue;
        }

        if !can_send_video_packet_to_decoder(app_state, *pts) {
            trace!(
                pts_ms = pts.as_millis(),
                audio_ms = app_state.audio_clock_now().as_millis(),
                max_ahead_ms = MAX_VIDEO_DECODE_AHEAD.as_millis(),
                "A/V sync: holding video packet to limit decode-ahead"
            );
            break;
        }

        let Some((track_id, pts, data, keyframe)) = app_state.pending_video_packets.pop_front()
        else {
            break;
        };

        trace!(
            pts_ms = pts.as_millis(),
            data_len = data.len(),
            keyframe,
            "Sending video packet to decoder thread"
        );

        let packet = video_vaapi::DecodePacket {
            track_id,
            pts,
            data,
            keyframe,
        };

        if let Err(e) = thread.send_packet(packet) {
            tracing::warn!(error = %e, "Failed to send packet to decoder thread");
            break;
        }

        sent_packets += 1;
    }
}

/// Удаляет лишние кадры, если presentation queue стала больше безопасного лимита.
fn trim_video_present_queue(app_state: &mut AppState, telemetry: &Telemetry) {
    while app_state.video_frame_queue.len() > MAX_VIDEO_PRESENT_QUEUE {
        if let Some(frame) = app_state.video_frame_queue.pop_front() {
            if let Some(ref thread) = app_state.video_decoder_thread {
                thread.release_frame(frame.texture_handle);
            }
            telemetry.record_video_frame_dropped(VideoDropReason::QueueOverflow);
            tracing::debug!(
                pts_ms = frame.pts.as_millis(),
                "Dropping frame: queue overflow protection"
            );
        }
    }
}

/// Добавляет duration без panic при переполнении.
fn saturating_duration_add(
    timestamp: std::time::Duration,
    offset: std::time::Duration,
) -> std::time::Duration {
    timestamp
        .checked_add(offset)
        .unwrap_or(std::time::Duration::MAX)
}

/// Рассчитывает media time, под который выбираем frame для ближайшего present.
fn target_media_time_for_present(
    app_state: &AppState,
    audio_now: std::time::Duration,
) -> std::time::Duration {
    let present_lead = app_state
        .video_frame_duration_estimate
        .mul_f64(VIDEO_PRESENT_LEAD_FRAMES);
    saturating_duration_add(audio_now, present_lead)
}

/// Возвращает допустимое окно выбора кадра вокруг target media time.
fn video_present_window(app_state: &AppState) -> std::time::Duration {
    app_state
        .video_frame_duration_estimate
        .mul_f64(VIDEO_PRESENT_WINDOW_FRAMES)
}

/// Возвращает допустимое опоздание кадра перед forced catch-up drop.
fn video_late_drop_grace(app_state: &AppState) -> std::time::Duration {
    app_state
        .video_frame_duration_estimate
        .mul_f64(VIDEO_LATE_DROP_GRACE_FRAMES)
}

/// Проверяет, нужно ли дропнуть первый queued frame как реально устаревший.
fn should_drop_front_frame_as_late(
    app_state: &AppState,
    target_media_time: std::time::Duration,
    late_drop_grace: std::time::Duration,
) -> bool {
    let Some(front_frame) = app_state.video_frame_queue.front() else {
        return false;
    };
    let Some(next_frame) = app_state.video_frame_queue.get(1) else {
        return false;
    };

    let latest_front_pts = saturating_duration_add(front_frame.pts, late_drop_grace);
    if target_media_time <= latest_front_pts {
        return false;
    }

    next_frame.pts <= target_media_time
}

/// Проверяет, слишком ли рано показывать первый queued frame.
fn should_wait_for_front_frame(
    frame_pts: std::time::Duration,
    target_media_time: std::time::Duration,
    present_window: std::time::Duration,
) -> bool {
    frame_pts > saturating_duration_add(target_media_time, present_window)
}

/// Освобождает texture handle через decoder thread, если он ещё существует.
fn release_video_texture(app_state: &AppState, texture_handle: video_core::FrameTextureHandle) {
    if let Some(ref thread) = app_state.video_decoder_thread {
        thread.release_frame(texture_handle);
    }
}

/// Удаляет первый queued frame и записывает причину drop.
fn drop_front_queued_video_frame(
    app_state: &mut AppState,
    telemetry: &Telemetry,
    reason: VideoDropReason,
) -> bool {
    let Some(frame) = app_state.video_frame_queue.pop_front() else {
        return false;
    };

    let frame_pts_ms = frame.pts.as_millis();
    release_video_texture(app_state, frame.texture_handle);
    telemetry.record_video_frame_dropped(reason);
    tracing::debug!(
        pts_ms = frame_pts_ms,
        ?reason,
        "Dropping queued video frame"
    );
    true
}

/// Делает первый queued frame текущим present frame.
fn present_front_queued_video_frame(app_state: &mut AppState, telemetry: &Telemetry) -> bool {
    let Some(frame) = app_state.video_frame_queue.pop_front() else {
        return false;
    };

    if let Some(old_frame) = app_state.present_video_frame.take() {
        release_video_texture(app_state, old_frame.texture_handle);
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        "Presenting scheduled video frame"
    );
    app_state.present_video_frame = Some(frame);
    telemetry.record_video_frame_presented();
    true
}

/// Повторно показывает текущий кадр и учитывает это в telemetry.
fn repeat_present_video_frame(app_state: &AppState, telemetry: &Telemetry) {
    if app_state.present_video_frame.is_some() {
        telemetry.record_video_frame_repeated();
    }
}

/// Обрабатывает pending video packets: приём готовых кадров, backpressure и A/V sync.
fn process_pending_video_packets(app_state: &mut AppState, telemetry: &Telemetry) {
    // 1. Сначала забираем уже готовые кадры, чтобы decoder thread не забил frame channel.
    drain_decoded_video_frames(app_state, telemetry);

    let playback_can_present = app_state.player_state == crate::state::PlayerState::Playing
        || app_state.draining_after_eof;

    // 2. В пользовательской pause нельзя менять present frame и нельзя отправлять новые packets.
    if !playback_can_present {
        return;
    }

    // 3. При обычном playback отправляем в decoder небольшой realtime-бюджет video packets.
    if app_state.player_state == crate::state::PlayerState::Playing {
        send_pending_video_packets_to_decoder(app_state);
    }

    // 4. Защита от переполнения texture pool: не даём decoder'у убежать слишком далеко.
    // Старое значение 4 кадра было слишком маленьким и вызывало лишние drop/skip при 4K readback.
    trim_video_present_queue(app_state, telemetry);

    // 5. A/V sync: выбираем кадр через audio-master scheduler.
    if !app_state.video_frame_queue.is_empty() {
        tracing::debug!(
            queue_len = app_state.video_frame_queue.len(),
            "A/V sync: processing frame queue"
        );
    }
    // Флаг: audio playback остановлен (buffer underrun / EOF).
    // Важно: render loop может быть быстрее CPAL callback, поэтому stalled определяется
    // по времени с последнего изменения clock, а не по одному повторившемуся значению.
    let audio_now = app_state.audio_clock_now();
    let audio_ms = audio_now.as_secs_f64() * 1000.0;
    let now = std::time::Instant::now();
    if audio_now != app_state.last_audio_clock {
        app_state.last_audio_clock = audio_now;
        app_state.last_audio_clock_change_at = now;
    }
    let audio_stalled = audio_ms > 100.0
        && now.duration_since(app_state.last_audio_clock_change_at) >= AUDIO_STALL_TIMEOUT;
    if audio_stalled {
        tracing::debug!(
            audio_ms,
            stalled_ms = now
                .duration_since(app_state.last_audio_clock_change_at)
                .as_millis(),
            queue_len = app_state.video_frame_queue.len(),
            "A/V sync: audio stalled"
        );
    }

    // При stalled audio показываем максимум один кадр за redraw, чтобы EOF drain не завис.
    if audio_stalled {
        if !present_front_queued_video_frame(app_state, telemetry) {
            repeat_present_video_frame(app_state, telemetry);
        }
        return;
    }

    let target_media_time = target_media_time_for_present(app_state, audio_now);
    let present_window = video_present_window(app_state);
    let late_drop_grace = video_late_drop_grace(app_state);

    // Дропаем только реальный backlog. Небольшой phase jitter должен идти через
    // sequential present, иначе 60 FPS поток начинает регулярно терять кадры.
    while should_drop_front_frame_as_late(app_state, target_media_time, late_drop_grace) {
        if !drop_front_queued_video_frame(app_state, telemetry, VideoDropReason::Late) {
            break;
        }
    }

    let Some(frame) = app_state.video_frame_queue.front() else {
        repeat_present_video_frame(app_state, telemetry);
        return;
    };

    let diff_ms = frame.pts.as_secs_f64() * 1000.0 - target_media_time.as_secs_f64() * 1000.0;
    if should_wait_for_front_frame(frame.pts, target_media_time, present_window) {
        trace!(
            pts_ms = frame.pts.as_millis(),
            target_ms = target_media_time.as_millis(),
            diff_ms,
            window_ms = present_window.as_millis(),
            "A/V scheduler: waiting for target media time"
        );
        repeat_present_video_frame(app_state, telemetry);
        return;
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        audio_ms = audio_now.as_millis(),
        target_ms = target_media_time.as_millis(),
        diff_ms,
        window_ms = present_window.as_millis(),
        "A/V scheduler: frame selected"
    );
    present_front_queued_video_frame(app_state, telemetry);
}

/// Рендерит один полный кадр: видео + egui overlay.
///
/// Измеряет время кадра, обновляет телеметрию,
/// и вызывает renderer.render_frame().
#[instrument(skip(telemetry, window, renderer, app_state))]
fn render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
) {
    let frame_start = std::time::Instant::now();

    // Получаем ввод от egui_winit
    let egui_input = app_state.egui_winit_state.take_egui_input(window);

    // 1. Demux new packets только если playing.
    // При EOF переходим в Paused, но НЕ очищаем очередь кадров —
    // A/V sync должен успеть отобразить оставшиеся decoded frames.
    if app_state.player_state == crate::state::PlayerState::Playing {
        if app_state.demuxer.is_some() {
            for _ in 0..MAX_DEMUX_PACKETS_PER_REDRAW {
                if !can_read_next_demux_packet(app_state) {
                    trace!(
                        pending_video_packets = app_state.pending_video_packets.len(),
                        queued_video_frames = app_state.video_frame_queue.len(),
                        "Demux backpressure: waiting for decoder/presentation"
                    );
                    break;
                }

                let packet_result = {
                    let Some(demuxer) = app_state.demuxer.as_mut() else {
                        break;
                    };
                    demuxer.next_packet()
                };

                match packet_result {
                    Ok(Some(packet)) => {
                        telemetry.record_packet(packet.kind, packet.pts);

                        if packet.kind == webm_demux::TrackKind::Audio {
                            app_state
                                .pending_audio_packets
                                .push_back((packet.track_id, packet.data.to_vec()));
                        }
                        if packet.kind == webm_demux::TrackKind::Video {
                            app_state.pending_video_packets.push_back((
                                packet.track_id,
                                packet.pts,
                                packet.data.to_vec(),
                                packet.keyframe,
                            ));
                        }

                        if telemetry.packets_read() <= 50 {
                            tracing::debug!(
                                track_id = packet.track_id,
                                kind = ?packet.kind,
                                pts_ms = packet.pts.as_millis(),
                                size = packet.data.len(),
                                keyframe = packet.keyframe,
                                "Packet"
                            );
                        }
                    }
                    Ok(None) => {
                        // EOF: останавливаем demux, но оставляем frames для A/V sync.
                        app_state.draining_after_eof = true;
                        app_state.player_state = crate::state::PlayerState::Paused;
                        app_state.demuxer = None; // освобождаем файл
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Ошибка чтения packet");
                        break;
                    }
                }
            }

            // Process pending audio packets с throttle по buffer level
            app_state.process_pending_audio_packets();
        }
    }

    // 2. Всегда обрабатываем decoder output: отправка pending packets + приём готовых кадров + A/V sync.
    // Это критично после EOF — decoder thread может ещё выдавать кадры из внутренней очереди.
    process_pending_video_packets(app_state, telemetry);

    // Рендерим egui UI — получаем paint jobs и texture updates
    let egui_full_output = app_state.render_ui(window, egui_input);

    // Обработка platform output (курсор, буфер обмена и т.д.)
    app_state
        .egui_winit_state
        .handle_platform_output(window, egui_full_output.platform_output);

    // Тесселяция egui paint jobs в примитивы для wgpu
    let pixels_per_point = app_state.egui_ctx.pixels_per_point();
    let paint_jobs = app_state
        .egui_ctx
        .tessellate(egui_full_output.shapes, pixels_per_point);

    // Screen descriptor для egui_wgpu
    let size = window.inner_size();
    let screen_descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [size.width.max(1), size.height.max(1)],
        pixels_per_point,
    };

    // Получаем wgpu texture views для decoded frame если decoder thread доступен.
    let (video_y_view, video_uv_view): (Option<wgpu::TextureView>, Option<wgpu::TextureView>) = {
        if let Some(ref thread) = app_state.video_decoder_thread {
            if let Some(ref frame) = app_state.present_video_frame {
                tracing::trace!(
                    handle_id = frame.texture_handle.0,
                    pts_ms = frame.pts.as_millis(),
                    "Getting texture views for present frame"
                );
                match thread.get_views(frame.texture_handle) {
                    Some((y_view, uv_view)) => {
                        trace!("Texture views acquired — WILL render NV12 video");
                        (Some(y_view), Some(uv_view))
                    }
                    None => {
                        tracing::error!(
                            handle_id = frame.texture_handle.0,
                            "Texture views not found for handle"
                        );
                        (None, None)
                    }
                }
            } else {
                trace!("No present_video_frame — nothing to render yet");
                (None, None)
            }
        } else {
            trace!("No video decoder thread — nothing to render yet");
            (None, None)
        }
    };

    // Время для анимации синтетического видео
    let time = app_state.elapsed_seconds() as f32;

    // Рендерим полный кадр (видео + egui overlay)
    renderer.render_frame(
        window,
        time,
        app_state.present_video_frame.as_ref(),
        video_y_view.as_ref(),
        video_uv_view.as_ref(),
        paint_jobs,
        egui_full_output.textures_delta,
        screen_descriptor,
    );

    // Измеряем время кадра и обновляем телеметрию
    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
}

/// Точка входа приложения.
///
/// Инициализирует:
/// - tracing для логирования
/// - winit event loop
/// - App и запускает run_app
fn main() -> Result<()> {
    // Инициализируем tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    info!("=== YouTube Player Stage 1: Render Shell ===");
    info!("Запуск приложения");

    // Создаём event loop
    let event_loop = winit::event_loop::EventLoop::builder()
        .build()
        .context("Не удалось создать event loop")?;

    // ControlFlow::Poll — непрерывный рендеринг (vsync контролируется present mode)
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    // Создаём и запускаем приложение
    let (initial_media, initial_error) = resolve_initial_media_from_cli();
    if let Some(InitialMedia::File(path)) = &initial_media {
        info!(path = %path.display(), "CLI аргумент: файл для воспроизведения");
    }
    let mut app = App::new(initial_media, initial_error);
    event_loop.run_app(&mut app)?;

    info!("Приложение завершено");
    Ok(())
}

/// Подготавливает стартовый media-файл из CLI-аргумента.
///
/// Локальный путь возвращается как файл.
/// HTTP URL открывается через streaming adapter.
fn resolve_initial_media_from_cli() -> (Option<InitialMedia>, Option<String>) {
    // Берём только первый пользовательский аргумент, чтобы не вводить неполный CLI parser.
    let Some(argument) = std::env::args().nth(1) else {
        return (None, None);
    };

    // URL обрабатываем отдельно: текущий demuxer умеет только локальные файлы.
    if youtube::is_probably_url(&argument) {
        info!(url = %argument, "CLI аргумент распознан как YouTube/web URL");

        return match youtube::open_streaming_media(&argument) {
            Ok(streaming_media) => {
                info!(
                    description = %streaming_media.description,
                    "YouTube media подготовлен для streaming playback"
                );
                (
                    Some(InitialMedia::Streaming {
                        label: streaming_media.description,
                        demuxer: streaming_media.demuxer,
                    }),
                    None,
                )
            }
            Err(error) => {
                warn!(error = %error, "Не удалось подготовить YouTube URL");
                (None, Some(format!("YouTube error: {error}")))
            }
        };
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(argument))), None)
}
