/// Окно приложения и главный цикл рендеринга.
///
/// Этот модуль отвечает за:
/// - создание окна через winit
/// - обработку событий окна (ресайз, закрытие, ввод)
/// - запуск egui для UI overlay
/// - поддержание 60 fps через VSync (Fifo present mode)
/// - координацию между рендерингом видео и UI
///
/// Почему winit + render-wgpu, а не eframe:
/// eframe скрывает lifecycle и swapchain детали, а нам нужен явный контроль
/// над окном и shared GPU context для zero-copy video path. Сам render pass
/// теперь живёт в `render-wgpu`, а app shell только передаёт input/snapshot.
///
/// Архитектура event loop:
/// - winit 0.30 использует ApplicationHandler trait
/// - события: Resumed (создание окна), Suspended (уничтожение), WindowEvent
/// - RedrawRequested — основной hook для рендеринга каждого кадра
mod state;
mod telemetry;
mod ui;

use std::path::PathBuf;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use capability_core::CapabilityScanner;
use player_core::{PlayerRenderError, PlayerTickResult, PlayerVideoDropReason, PlayerWorkerEvent};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrOutputMode, HdrToSdrSettings,
    HdrToneMappingOperator, RenderCapabilities, SwapchainTransferMode,
};
use render_wgpu::{RenderFrameOutcome, Renderer};
use rustiplayer_config::{AppConfig, HdrToSdrOperatorConfig};
use tracing::{debug, info, instrument, warn};
use video_core::DecodedPixelFormat;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow},
    window::Window,
};

use crate::state::AppState;
use crate::telemetry::{Telemetry, VideoDropReason};

/// Интервал polling-а фоновой подготовки YouTube, когда playback ещё не активен.
const YOUTUBE_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Решение render pass-а о том, нужен ли следующий redraw без внешнего window event-а.
#[derive(Debug, Clone, Copy)]
struct RedrawPacing {
    /// Playback/seek/opening state требует непрерывного render loop-а.
    continuous: bool,

    /// Worker command или egui попросили ещё один redraw для доставки нового состояния.
    follow_up: bool,
}

impl RedrawPacing {
    /// Возвращает `true`, если следующий redraw надо запросить сразу.
    const fn wants_immediate_redraw(self) -> bool {
        self.continuous || self.follow_up
    }
}

/// Media, которое нужно автоматически открыть после создания окна.
enum InitialMedia {
    /// Локальный файл.
    File(PathBuf),

    /// YouTube/web URL, который нужно подготовить после старта UI.
    YouTubeUrl { url: String },
}

/// Результат фоновой подготовки CLI YouTube URL.
type YoutubeStartupResult = std::result::Result<service_youtube::YoutubeStreamingMedia, String>;

/// Фоновый job, который не блокирует создание окна и UI.
struct YoutubeStartupJob {
    /// URL страницы/ролика, который был передан через CLI.
    source_url: String,

    /// Текст pending-состояния для центрального overlay.
    pending_message: String,

    /// Receiver одноразового результата background resolver-а.
    result_rx: Receiver<YoutubeStartupResult>,

    /// JoinHandle нужен для cleanup после получения результата.
    join_handle: Option<JoinHandle<()>>,
}

impl YoutubeStartupJob {
    /// Запускает подготовку YouTube media на отдельном thread-е.
    fn spawn(source_url: String, app_config: AppConfig) -> std::result::Result<Self, String> {
        let (result_tx, result_rx) = mpsc::channel();
        let thread_url = source_url.clone();
        let network_config = app_config.network.clone();
        let youtube_config = app_config.youtube.clone();
        let demux_config = app_config.player.demux;
        let join_handle = thread::Builder::new()
            .name("youtube-startup-resolver".to_string())
            .spawn(move || {
                let resolve_result = service_youtube::open_streaming_media_with_demux_config(
                    &thread_url,
                    &network_config,
                    &youtube_config,
                    &demux_config,
                )
                .map_err(|error| format!("{error:#}"));

                if result_tx.send(resolve_result).is_err() {
                    debug!("UI больше не ждёт результат YouTube startup resolver-а");
                }
            })
            .map_err(|error| format!("Не удалось запустить YouTube startup resolver: {error}"))?;

        Ok(Self {
            source_url,
            pending_message: "Подготовка YouTube stream...".to_string(),
            result_rx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает pending-текст без доступа к внутреннему channel state.
    fn pending_message(&self) -> &str {
        &self.pending_message
    }

    /// Неблокирующе забирает результат resolver-а, если он уже готов.
    fn try_take_result(&mut self) -> Option<YoutubeStartupResult> {
        let result = match self.result_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("YouTube startup resolver завершился без результата".to_string())
            }
        };

        if let Some(join_handle) = self.join_handle.take()
            && join_handle.join().is_err()
        {
            return Some(Err(
                "YouTube startup resolver завершился panic после результата".to_string(),
            ));
        }

        Some(result)
    }
}

/// Главное приложение — реализует ApplicationHandler для winit.
///
/// Владеет:
/// - окном (Arc<Window>)
/// - рендерером (`render-wgpu` backend)
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

    /// Фоновая подготовка CLI YouTube URL, если она уже запущена.
    youtube_startup_job: Option<YoutubeStartupJob>,

    /// Startup-ошибка shell-слоя, которую нужно показать после создания UI.
    startup_error: Option<String>,

    /// Валидированная пользовательская конфигурация.
    app_config: AppConfig,

    /// Ближайшее время, когда нужно снова проверить background YouTube job.
    next_youtube_startup_poll_at: Option<Instant>,
}

impl App {
    /// Создаёт пустое приложение.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    fn new(
        initial_media: Option<InitialMedia>,
        startup_error: Option<String>,
        app_config: AppConfig,
    ) -> Self {
        Self {
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            initial_media,
            youtube_startup_job: None,
            startup_error,
            app_config,
            next_youtube_startup_poll_at: None,
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

        let mut renderer = match Renderer::new(window.clone()) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::error!("Не удалось инициализировать рендерер: {}", error);
                event_loop.exit();
                return;
            }
        };
        let color_pipeline_settings = match color_pipeline_settings_from_config(&self.app_config) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::error!(error = %error, "Некорректные render color settings");
                event_loop.exit();
                return;
            }
        };
        let hdr_to_sdr_settings = hdr_to_sdr_settings_from_config(&self.app_config);
        warn_legacy_tone_mapping_config(&self.app_config);
        renderer.set_color_pipeline_settings(color_pipeline_settings);
        renderer.set_hdr_to_sdr_settings(hdr_to_sdr_settings);

        let mut app_state = match AppState::new(
            &window,
            self.telemetry.clone(),
            self.app_config.clone(),
            self.startup_error.clone(),
        ) {
            Ok(app_state) => app_state,
            Err(error) => {
                tracing::error!(error = %error, "Не удалось запустить app state");
                event_loop.exit();
                return;
            }
        };
        let system_capabilities = probe_system_capabilities(renderer.render_capabilities());
        info!("{}", system_capabilities.summary_text());
        app_state.set_system_capabilities(system_capabilities);
        app_state.init_video_pipeline(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );

        if let Some(job) = &self.youtube_startup_job {
            app_state.set_startup_pending(job.pending_message().to_string());
        }

        if let Some(initial_media) = self.initial_media.take() {
            match initial_media {
                InitialMedia::File(path) => {
                    info!(path = %path.display(), "Автозагрузка файла из CLI");
                    app_state.load_file(&path);
                }
                InitialMedia::YouTubeUrl { url } => {
                    info!(source = %url, "Автозагрузка YouTube URL из CLI");
                    self.start_youtube_startup_job(url, &mut app_state);
                }
            }
        }
        poll_youtube_startup_job(
            &mut self.youtube_startup_job,
            &mut app_state,
            &mut self.startup_error,
        );
        self.refresh_youtube_startup_poll_deadline();

        // Shell получает read-only snapshot без доступа к mutable playback internals.
        let _player_snapshot = app_state.player_snapshot();

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn drop_runtime(&mut self) {
        if let Some(app_state) = &self.app_state
            && let Some(path) = app_state.current_local_file()
        {
            self.initial_media = Some(InitialMedia::File(path.to_path_buf()));
            self.startup_error = None;
        }
        self.app_state = None;
        self.renderer = None;
    }

    /// Запускает background resolve для CLI YouTube URL и сразу обновляет UI state.
    fn start_youtube_startup_job(&mut self, source_url: String, app_state: &mut AppState) {
        app_state.set_startup_pending("Подготовка YouTube stream...".to_string());
        self.next_youtube_startup_poll_at = None;
        match YoutubeStartupJob::spawn(source_url, self.app_config.clone()) {
            Ok(job) => {
                self.startup_error = None;
                self.youtube_startup_job = Some(job);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить YouTube startup resolver");
                let startup_error = format!("NetworkError: YouTube error: {error}");
                self.startup_error = Some(startup_error.clone());
                app_state.set_startup_error(startup_error);
                self.next_youtube_startup_poll_at = None;
            }
        }
    }

    /// Сбрасывает deadline polling-а, когда background startup job уже завершён.
    fn refresh_youtube_startup_poll_deadline(&mut self) {
        if self.youtube_startup_job.is_none() {
            self.next_youtube_startup_poll_at = None;
        }
    }

    /// Применяет pacing после render pass-а.
    fn apply_redraw_pacing(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        pacing: RedrawPacing,
    ) {
        if pacing.continuous {
            event_loop.set_control_flow(ControlFlow::Poll);
            window.request_redraw();
            return;
        }

        if pacing.wants_immediate_redraw() {
            event_loop.set_control_flow(ControlFlow::Wait);
            window.request_redraw();
            return;
        }

        self.configure_idle_control_flow(event_loop);
    }

    /// Настраивает idle ожидание: обычный Wait или timed wakeup для background startup.
    fn configure_idle_control_flow(&mut self, event_loop: &ActiveEventLoop) {
        if self.youtube_startup_job.is_none() {
            self.next_youtube_startup_poll_at = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let deadline = self
            .next_youtube_startup_poll_at
            .unwrap_or_else(|| Instant::now() + YOUTUBE_STARTUP_POLL_INTERVAL);
        self.next_youtube_startup_poll_at = Some(deadline);
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }

    /// Возвращает `true`, если shell уже держит continuous redraw из-за playback.
    fn has_continuous_redraw(&self) -> bool {
        self.app_state
            .as_ref()
            .is_some_and(AppState::wants_continuous_redraw)
    }
}

/// Забирает готовый результат фоновой подготовки YouTube и доставляет его в UI/player.
fn poll_youtube_startup_job(
    job_slot: &mut Option<YoutubeStartupJob>,
    app_state: &mut AppState,
    startup_error_slot: &mut Option<String>,
) {
    let Some(job) = job_slot.as_mut() else {
        return;
    };
    let Some(resolve_result) = job.try_take_result() else {
        return;
    };
    let source_url = job.source_url.clone();
    *job_slot = None;

    match resolve_result {
        Ok(streaming_media) => {
            *startup_error_slot = None;
            info!(
                source = %source_url,
                description = %streaming_media.description,
                "YouTube media подготовлен для streaming playback"
            );
            app_state.load_youtube_demuxer(streaming_media.description, streaming_media.demuxer);
        }
        Err(error) => {
            warn!(source = %source_url, error = %error, "Не удалось подготовить YouTube URL");
            let startup_error = format!("NetworkError: YouTube error: {error}");
            *startup_error_slot = Some(startup_error.clone());
            app_state.set_startup_error(startup_error);
        }
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
        let Some(window) = self.window.clone() else {
            return;
        };
        let (Some(renderer), Some(app_state)) = (&mut self.renderer, &mut self.app_state) else {
            return;
        };

        // Передаём событие в egui_winit для обработки ввода
        let egui_response = app_state.egui_winit_state.on_window_event(&window, &event);
        let redraw_after_event = should_request_redraw_after_window_event(&event);

        // Escape во время pointer scrub завершает scrub, а не закрывает окно.
        let escape_pressed = matches!(
            &event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Code(
                        winit::keyboard::KeyCode::Escape
                    ),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
                ..
            }
        );
        if escape_pressed && app_state.cancel_active_timeline_scrub() {
            window.request_redraw();
            return;
        }

        // Если egui потребил событие (например, клик по кнопке), не обрабатываем дальше
        if egui_response.consumed {
            if redraw_after_event {
                window.request_redraw();
            }
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                info!("Закрытие окна по запросу пользователя");
                self.drop_runtime();
                event_loop.exit();
                return;
            }

            WindowEvent::Resized(PhysicalSize { width, height }) => {
                debug!(width, height, "Изменение размера окна");
                renderer.resize(width, height);
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
                    return;
                }
                other => {
                    app_state.handle_hotkeys(&window, other, egui_response.consumed);
                }
            },

            WindowEvent::RedrawRequested => {
                poll_youtube_startup_job(
                    &mut self.youtube_startup_job,
                    app_state,
                    &mut self.startup_error,
                );
                let pacing = render_frame(&self.telemetry, &window, renderer, app_state);
                self.refresh_youtube_startup_poll_deadline();
                self.apply_redraw_pacing(event_loop, &window, pacing);
                return;
            }

            _ => {}
        }

        if redraw_after_event {
            window.request_redraw();
        }
    }

    /// Перед idle wait будит shell только для timed background job polling-а.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.has_continuous_redraw() {
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }

        if self.youtube_startup_job.is_none() {
            self.next_youtube_startup_poll_at = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let now = Instant::now();
        let deadline = self.next_youtube_startup_poll_at.unwrap_or(now);
        if now >= deadline {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            self.next_youtube_startup_poll_at = Some(now + YOUTUBE_STARTUP_POLL_INTERVAL);
        }

        if let Some(next_deadline) = self.next_youtube_startup_poll_at {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next_deadline));
        }
    }
}

/// Возвращает `true`, если window/input событие должно дать один redraw в Wait mode.
fn should_request_redraw_after_window_event(event: &WindowEvent) -> bool {
    !matches!(
        event,
        WindowEvent::RedrawRequested | WindowEvent::CloseRequested
    )
}

/// Переносит результат playback worker tick в shell telemetry.
fn record_player_tick_result(telemetry: &Telemetry, tick_result: &PlayerTickResult) {
    for packet in &tick_result.demuxed_packets {
        telemetry.record_packet(packet.kind);

        if telemetry.packets_read() <= 50 {
            tracing::debug!(
                track_id = %packet.track_id,
                kind = ?packet.kind,
                pts_ms = packet.pts.as_millis(),
                size = packet.size,
                keyframe = packet.keyframe,
                "Packet"
            );
        }
    }

    for _ in 0..tick_result.decoded_video_frames {
        telemetry.record_video_frame_decoded();
    }

    for _ in 0..tick_result.video_frames_presented {
        telemetry.record_video_frame_presented();
    }

    for _ in 0..tick_result.video_frames_repeated {
        telemetry.record_video_frame_repeated();
    }

    for dropped_frame in &tick_result.dropped_video_frames {
        telemetry.record_video_frame_dropped(map_video_drop_reason(dropped_frame.reason));
    }
}

/// Переносит worker event stream в shell telemetry.
fn record_worker_events(telemetry: &Telemetry, events: Vec<PlayerWorkerEvent>) {
    for event in events {
        match event {
            PlayerWorkerEvent::Tick(tick_result) => {
                record_player_tick_result(telemetry, &tick_result);
            }
            PlayerWorkerEvent::RenderError(_) => {}
            PlayerWorkerEvent::Player(_) => {}
        }
    }
}

/// Конвертирует core-причину drop в app telemetry enum.
fn map_video_drop_reason(reason: PlayerVideoDropReason) -> VideoDropReason {
    match reason {
        PlayerVideoDropReason::Late => VideoDropReason::Late,
        PlayerVideoDropReason::QueueOverflow => VideoDropReason::QueueOverflow,
        PlayerVideoDropReason::Paused => VideoDropReason::Paused,
        PlayerVideoDropReason::StaleGeneration
        | PlayerVideoDropReason::SeekPreroll
        | PlayerVideoDropReason::RenderAcquisitionTimeout
        | PlayerVideoDropReason::DecoderStarvation => VideoDropReason::Other,
    }
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
) -> RedrawPacing {
    let frame_start = Instant::now();

    // Получаем ввод от egui_winit
    let egui_input = app_state.egui_winit_state.take_egui_input(window);
    app_state.set_render_diagnostics(renderer.diagnostics());
    record_worker_events(telemetry, app_state.drain_worker_events());

    // Рендерим egui UI — получаем paint jobs и texture updates
    let egui_full_output = app_state.render_ui(window, egui_input);
    let egui_requested_repaint = app_state.egui_ctx.has_requested_repaint();

    // Обработка platform output (курсор, буфер обмена и т.д.)
    app_state
        .egui_winit_state
        .handle_platform_output(window, egui_full_output.platform_output);

    // Тесселяция egui paint jobs в примитивы для wgpu
    let pixels_per_point = app_state.egui_ctx.pixels_per_point();
    let paint_jobs = app_state
        .egui_ctx
        .tessellate(egui_full_output.shapes, pixels_per_point);

    // Размер и scale передаются renderer backend-у без прямой зависимости от egui-wgpu.
    let size = window.inner_size();
    let screen_size_in_pixels = [size.width.max(1), size.height.max(1)];

    let mut video_render_error = None;

    let present_frame_acquisition = app_state.acquire_present_frame_for_render();
    let present_frame_acquisition_state = present_frame_acquisition.metric_name();
    if present_frame_acquisition.reused_previous_frame() {
        telemetry.record_video_frame_repeated();
    }
    let present_frame = present_frame_acquisition.into_present_frame();

    let present_frame_texture_views = match present_frame.as_ref() {
        Some(present_frame) => match present_frame.texture_views() {
            Some(texture_views) => Some(texture_views),
            None => {
                video_render_error = Some(PlayerRenderError::missing_texture_views(present_frame));
                None
            }
        },
        None => None,
    };

    // Собираем renderer boundary frame только если lease metadata и обе texture planes готовы.
    let video_frame = match (present_frame.as_ref(), present_frame_texture_views.as_ref()) {
        (Some(present_frame), Some(texture_views)) => {
            tracing::trace!(
                handle_id = present_frame.frame.texture_handle.0,
                pts_ms = present_frame.frame.pts.as_millis(),
                format = %present_frame.frame.format,
                memory_path = %present_frame.frame.memory_path,
                stale = present_frame.stale,
                acquisition = present_frame_acquisition_state,
                "Present frame acquired from playback worker"
            );

            let boundary_frame = match present_frame.frame.format {
                DecodedPixelFormat::Nv12 => render_wgpu::WgpuRenderableFrame::from_decoded_nv12(
                    &present_frame.frame,
                    &texture_views.y_view,
                    &texture_views.uv_view,
                ),
                DecodedPixelFormat::P010 => render_wgpu::WgpuRenderableFrame::from_decoded_p010(
                    &present_frame.frame,
                    &texture_views.y_view,
                    &texture_views.uv_view,
                ),
                DecodedPixelFormat::Rgba8 => Err(anyhow::anyhow!(
                    "RGBA8 decoded video surface is not a production zero-copy render path"
                )),
            };

            match boundary_frame {
                Ok(boundary_frame) => Some(boundary_frame),
                Err(error) => {
                    let message = format!(
                        "WGPU renderable frame rejected decoded {} frame: {}",
                        present_frame.frame.format, error
                    );
                    tracing::error!(
                        error = %error,
                        format = %present_frame.frame.format,
                        memory_path = %present_frame.frame.memory_path,
                        "Failed to build WGPU renderable frame"
                    );
                    video_render_error = Some(PlayerRenderError::unsupported_frame_format(
                        present_frame,
                        message,
                    ));
                    None
                }
            }
        }
        _ => None,
    };

    if let Some(error) = video_render_error {
        app_state.report_render_error(error);
        tracing::debug!(
            acquisition = "render_error_reported",
            "Present frame acquisition ended with render boundary error"
        );
    }

    // Рендерим полный кадр (видео + egui overlay)
    match renderer.render_frame(render_wgpu::RenderFrameInput {
        window,
        video_frame: video_frame.as_ref(),
        egui_paint_jobs: paint_jobs,
        egui_textures_delta: egui_full_output.textures_delta,
        screen: render_wgpu::RenderScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        },
    }) {
        RenderFrameOutcome::Presented => telemetry.record_presented_frame(),
        RenderFrameOutcome::Dropped(_reason) => telemetry.record_dropped_frame(),
        RenderFrameOutcome::Failed(failure) => {
            telemetry.record_dropped_frame();
            app_state.report_render_error(PlayerRenderError::render_device_lost(format!(
                "Video render failed: {}",
                failure.message
            )));
        }
    }
    app_state.set_render_diagnostics(renderer.diagnostics());

    // Измеряем время кадра и обновляем телеметрию
    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);

    RedrawPacing {
        continuous: app_state.wants_continuous_redraw(),
        follow_up: app_state.take_pending_worker_redraw() || egui_requested_repaint,
    }
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

    let loaded_config =
        rustiplayer_config::load_or_create().context("Не удалось загрузить config rustiplayer")?;
    info!(
        path = %loaded_config.path.display(),
        created = loaded_config.created,
        "Config rustiplayer готов"
    );

    // Создаём event loop
    let event_loop = winit::event_loop::EventLoop::builder()
        .build()
        .context("Не удалось создать event loop")?;

    // Idle default — Wait; playback включает Poll только на активном render loop-е.
    event_loop.set_control_flow(ControlFlow::Wait);

    let (initial_media, cli_startup_error) = resolve_initial_media_from_cli(&loaded_config.config);
    if let Some(InitialMedia::File(path)) = &initial_media {
        info!(path = %path.display(), "CLI аргумент: файл для воспроизведения");
    }
    let mut app = App::new(initial_media, cli_startup_error, loaded_config.config);
    event_loop.run_app(&mut app)?;

    info!("Приложение завершено");
    Ok(())
}

/// Подготавливает стартовый media-файл из CLI-аргумента.
///
/// Локальный путь возвращается как файл.
/// HTTP URL открывается через streaming adapter.
fn resolve_initial_media_from_cli(
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    // Берём только первый пользовательский аргумент, чтобы не вводить неполный CLI parser.
    let Some(argument) = std::env::args().nth(1) else {
        return (None, None);
    };

    // URL обрабатываем отдельно: текущий demuxer умеет только локальные файлы.
    if service_youtube::is_probably_url(&argument) {
        info!(url = %argument, "CLI аргумент распознан как YouTube/web URL");
        if !app_config.youtube.enabled {
            return (
                None,
                Some("NetworkError: YouTube adapter отключён в config".to_string()),
            );
        }

        return (Some(InitialMedia::YouTubeUrl { url: argument }), None);
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(argument))), None)
}

/// Запускает compile-time зарегистрированные capability probes.
fn probe_system_capabilities(
    render_capabilities: RenderCapabilities,
) -> capability_core::SystemCapabilities {
    let mut scanner = CapabilityScanner::new();
    scanner.register_provider(Box::new(video_vaapi::VaapiCapabilityProvider::new()));
    scanner.register_render_capabilities(render_capabilities);
    scanner.scan()
}

/// Логирует legacy tone mapping placeholder, который Phase 10 не превращает в UI preset.
fn warn_legacy_tone_mapping_config(app_config: &AppConfig) {
    let tone_mapping_is_disabled =
        app_config.render.tone_mapping == rustiplayer_config::ToneMappingMode::Disabled;

    if tone_mapping_is_disabled {
        return;
    }

    warn!(
        tone_mapping = ?app_config.render.tone_mapping,
        "Legacy tone_mapping config не применяется как alternative HDR control в Phase 10"
    );
}

/// Собирает HDR-to-SDR renderer settings из валидированного пользовательского config.
fn hdr_to_sdr_settings_from_config(app_config: &AppConfig) -> HdrToSdrSettings {
    let hdr_to_sdr = &app_config.render.hdr_to_sdr;

    HdrToSdrSettings {
        enabled: hdr_to_sdr.enabled,
        operator: hdr_to_sdr_operator_from_config(hdr_to_sdr.operator),
        output_mode: HdrOutputMode::SdrBt709Only,
        sdr_reference_white_nits: hdr_to_sdr.sdr_reference_white_nits,
        hdr_reference_peak_nits: hdr_to_sdr.hdr_reference_peak_nits,
    }
}

/// Мапит TOML operator в renderer contract без добавления alternative controls.
const fn hdr_to_sdr_operator_from_config(
    operator: HdrToSdrOperatorConfig,
) -> HdrToneMappingOperator {
    match operator {
        HdrToSdrOperatorConfig::Bt2446C => HdrToneMappingOperator::Bt2446C,
    }
}

/// Собирает renderer color settings из валидированного пользовательского config.
fn color_pipeline_settings_from_config(app_config: &AppConfig) -> Result<ColorPipelineSettings> {
    let color_adjustment = &app_config.render.color_adjustment;

    Ok(ColorPipelineSettings {
        adjustment: ColorAdjustment {
            brightness: color_adjustment.brightness,
            contrast: color_adjustment.contrast,
            saturation: color_adjustment.saturation,
            exposure: color_adjustment.exposure,
            rgb_gain: rgb_triplet_from_config(
                "render.color_adjustment.rgb_gain",
                &color_adjustment.rgb_gain,
            )?,
            rgb_offset: rgb_triplet_from_config(
                "render.color_adjustment.rgb_offset",
                &color_adjustment.rgb_offset,
            )?,
        },
        tone_mapping: render_core::ToneMappingMode::Off,
        swapchain_transfer: SwapchainTransferMode::PreserveCurrentUnorm,
    })
}

/// Конвертирует validated RGB list из config в fixed-size renderer contract.
fn rgb_triplet_from_config(field: &'static str, values: &[f32]) -> Result<[f32; 3]> {
    if values.len() != 3 {
        bail!(
            "{field} должен содержать ровно 3 значения, получено {}",
            values.len()
        );
    }

    for (channel_index, channel_value) in values.iter().copied().enumerate() {
        if !channel_value.is_finite() {
            bail!("{field}[{channel_index}] должен быть конечным числом, получено {channel_value}");
        }
    }

    Ok([values[0], values[1], values[2]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_wgpu::RenderFrameFailure;

    /// Проверяет, что identity config доезжает до renderer без изменения SDR картинки.
    #[test]
    fn default_config_maps_to_identity_color_pipeline_settings() {
        let settings =
            color_pipeline_settings_from_config(&AppConfig::default()).expect("settings mapped");

        assert_eq!(settings, ColorPipelineSettings::identity());
    }

    /// Проверяет, что `[render.hdr_to_sdr]` доезжает до renderer contract.
    #[test]
    fn default_config_maps_to_phase10_hdr_to_sdr_settings() {
        let settings = hdr_to_sdr_settings_from_config(&AppConfig::default());

        assert_eq!(settings, HdrToSdrSettings::default());
    }

    /// Проверяет, что renderer error становится fatal media error, а не silent fallback.
    #[test]
    fn render_failure_maps_to_fatal_render_device_error() {
        let failure = RenderFrameFailure::new("P010 HDR renderer rejected strict metadata");

        let error = PlayerRenderError::render_device_lost(format!(
            "Video render failed: {}",
            failure.message
        ))
        .to_player_error();

        assert_eq!(error.kind, player_core::PlayerErrorKind::RenderDeviceLost);
        assert!(
            error
                .message
                .contains("P010 HDR renderer rejected strict metadata")
        );
    }

    /// Проверяет, что ошибка renderer boundary не превращается в silent empty frame.
    #[test]
    fn render_boundary_failure_maps_to_fatal_render_format_error() {
        let error = PlayerRenderError {
            kind: player_core::PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(4),
            frame_handle: Some(9),
            message: "WGPU renderable frame rejected decoded P010 frame".into(),
        }
        .to_player_error();

        assert_eq!(
            error.kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
        assert!(
            error
                .message
                .contains("WGPU renderable frame rejected decoded P010 frame")
        );
    }
}
