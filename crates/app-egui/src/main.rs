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
use player_core::{
    PlayerError, PlayerErrorKind, PlayerTickContext, PlayerTickResult, PlayerVideoDropReason,
};
use rustiplayer_config::AppConfig;
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

    /// Валидированная пользовательская конфигурация.
    app_config: AppConfig,
}

impl App {
    /// Создаёт пустое приложение.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    fn new(
        initial_media: Option<InitialMedia>,
        initial_error: Option<String>,
        app_config: AppConfig,
    ) -> Self {
        Self {
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            initial_media,
            initial_error,
            app_config,
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

        let mut app_state = AppState::new(&window, self.telemetry.clone(), self.app_config.clone());
        app_state.init_video_pipeline(
            &renderer.gpu.instance,
            &renderer.gpu.adapter,
            &renderer.gpu.device,
            &renderer.gpu.queue,
        );

        if let Some(error) = self.initial_error.take() {
            app_state
                .player_session
                .mark_fatal_error(PlayerError::new(PlayerErrorKind::RuntimeError, error));
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
        }

        // Shell получает read-only snapshot без доступа к mutable playback internals.
        let _player_snapshot = app_state.player_snapshot();

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn drop_runtime(&mut self) {
        if let Some(app_state) = &self.app_state
            && let Some(path) = app_state.player_session.current_file_path()
        {
            self.initial_media = Some(InitialMedia::File(path.to_path_buf()));
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

/// Переносит результат `PlayerSession::tick` в shell telemetry.
fn record_player_tick_result(telemetry: &Telemetry, tick_result: &PlayerTickResult) {
    for packet in &tick_result.demuxed_packets {
        telemetry.record_packet(packet.kind, packet.pts);

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

/// Конвертирует core-причину drop в app telemetry enum.
fn map_video_drop_reason(reason: PlayerVideoDropReason) -> VideoDropReason {
    match reason {
        PlayerVideoDropReason::Late => VideoDropReason::Late,
        PlayerVideoDropReason::QueueOverflow => VideoDropReason::QueueOverflow,
        PlayerVideoDropReason::Paused => VideoDropReason::Paused,
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
) {
    let frame_start = std::time::Instant::now();

    // Получаем ввод от egui_winit
    let egui_input = app_state.egui_winit_state.take_egui_input(window);

    // Player tick продвигает demux/audio/video/scheduler, а shell только пишет telemetry.
    let tick_result = app_state
        .player_session
        .tick(PlayerTickContext::with_config(
            frame_start,
            app_state.tick_config(),
        ));
    record_player_tick_result(telemetry, &tick_result);

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
        if let Some(ref thread) = app_state.player_session.pipeline.video_decoder_thread {
            if let Some(ref frame) = app_state.player_session.pipeline.present_video_frame {
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
        app_state
            .player_session
            .pipeline
            .present_video_frame
            .as_ref(),
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

    // ControlFlow::Poll — непрерывный рендеринг (vsync контролируется present mode)
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    // Создаём и запускаем приложение
    let (initial_media, initial_error) = resolve_initial_media_from_cli();
    if let Some(InitialMedia::File(path)) = &initial_media {
        info!(path = %path.display(), "CLI аргумент: файл для воспроизведения");
    }
    let mut app = App::new(initial_media, initial_error, loaded_config.config);
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
