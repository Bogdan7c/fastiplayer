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

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use capability_core::CapabilityScanner;
use player_core::{
    PlayerError, PlayerErrorKind, PlayerTickContext, PlayerTickResult, PlayerVideoDropReason,
};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrOutputMode, HdrToSdrSettings,
    HdrToneMappingOperator, RenderCapabilities, SwapchainTransferMode,
};
use render_wgpu::{RenderFrameFailure, RenderFrameOutcome, Renderer};
use rustiplayer_config::{AppConfig, HdrToSdrOperatorConfig};
use rustiplayer_storage::StorageConnection;
use tracing::{debug, info, instrument, trace, warn};
use video_core::DecodedPixelFormat;
use winit::{
    application::ApplicationHandler, dpi::PhysicalSize, event::WindowEvent,
    event_loop::ActiveEventLoop, window::Window,
};

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

    /// Startup-ошибка shell-слоя, которую нужно показать после создания UI.
    startup_error: Option<String>,

    /// Валидированная пользовательская конфигурация.
    app_config: AppConfig,

    /// SQLite storage connection, открытый на время жизни приложения.
    storage_connection: Option<StorageConnection>,
}

impl App {
    /// Создаёт пустое приложение.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    fn new(
        initial_media: Option<InitialMedia>,
        startup_error: Option<String>,
        app_config: AppConfig,
        storage_connection: Option<StorageConnection>,
    ) -> Self {
        Self {
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            initial_media,
            startup_error,
            app_config,
            storage_connection,
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

        if self.storage_connection.is_none() {
            trace!("Storage connection недоступен; долговременные записи отключены");
        }

        let mut app_state = AppState::new(
            &window,
            self.telemetry.clone(),
            self.app_config.clone(),
            self.startup_error.clone(),
        );
        let system_capabilities = probe_system_capabilities(renderer.render_capabilities());
        info!("{}", system_capabilities.summary_text());
        app_state.set_system_capabilities(system_capabilities);
        app_state.init_video_pipeline(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );

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
    app_state.set_render_diagnostics(renderer.diagnostics());

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

    // Размер и scale передаются renderer backend-у без прямой зависимости от egui-wgpu.
    let size = window.inner_size();
    let screen_size_in_pixels = [size.width.max(1), size.height.max(1)];

    // Получаем wgpu texture views для decoded frame если decoder thread доступен.
    let (video_y_view, video_uv_view): (Option<wgpu::TextureView>, Option<wgpu::TextureView>) = {
        if let Some(ref thread) = app_state.player_session.pipeline.video_decoder_thread {
            if let Some(ref frame) = app_state.player_session.pipeline.present_video_frame {
                tracing::trace!(
                    handle_id = frame.texture_handle.0,
                    pts_ms = frame.pts.as_millis(),
                    format = %frame.format,
                    memory_path = %frame.memory_path,
                    "Getting texture views for present frame"
                );
                match thread.get_views(frame.texture_handle) {
                    Some((y_view, uv_view)) => {
                        trace!(format = %frame.format, "Texture views acquired for video frame");
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

    // Время оставлено в сигнатуре renderer-а для будущих diagnostics/animation hooks.
    let time = app_state.elapsed_seconds() as f32;

    // Собираем renderer boundary frame только если есть metadata и обе texture planes.
    let video_frame = match (
        app_state
            .player_session
            .pipeline
            .present_video_frame
            .as_ref(),
        video_y_view.as_ref(),
        video_uv_view.as_ref(),
    ) {
        (Some(frame), Some(y_view), Some(uv_view)) => {
            let boundary_frame = match frame.format {
                DecodedPixelFormat::Nv12 => {
                    render_wgpu::WgpuRenderableFrame::from_decoded_nv12(frame, y_view, uv_view)
                }
                DecodedPixelFormat::P010 => {
                    render_wgpu::WgpuRenderableFrame::from_decoded_p010(frame, y_view, uv_view)
                }
            };

            match boundary_frame {
                Ok(boundary_frame) => Some(boundary_frame),
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        format = %frame.format,
                        memory_path = %frame.memory_path,
                        "Failed to build WGPU renderable frame"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    // Рендерим полный кадр (видео + egui overlay)
    match renderer.render_frame(
        window,
        time,
        video_frame.as_ref(),
        paint_jobs,
        egui_full_output.textures_delta,
        screen_size_in_pixels,
        pixels_per_point,
    ) {
        RenderFrameOutcome::Presented => telemetry.record_presented_frame(),
        RenderFrameOutcome::Dropped(_reason) => telemetry.record_dropped_frame(),
        RenderFrameOutcome::Failed(failure) => {
            telemetry.record_dropped_frame();
            app_state
                .player_session
                .mark_fatal_error(player_error_from_render_failure(&failure));
        }
    }
    app_state.set_render_diagnostics(renderer.diagnostics());

    // Измеряем время кадра и обновляем телеметрию
    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
}

/// Переводит renderer failure в fatal media/runtime error без SDR fallback.
fn player_error_from_render_failure(failure: &RenderFrameFailure) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RenderDeviceLost,
        format!("Video render failed: {}", failure.message),
    )
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
    let (storage_connection, storage_startup_error) = initialize_storage();

    let (initial_media, cli_startup_error) = resolve_initial_media_from_cli();
    if let Some(InitialMedia::File(path)) = &initial_media {
        info!(path = %path.display(), "CLI аргумент: файл для воспроизведения");
    }
    let startup_error = combine_startup_errors([storage_startup_error, cli_startup_error]);
    let mut app = App::new(
        initial_media,
        startup_error,
        loaded_config.config,
        storage_connection,
    );
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
    if service_youtube::is_probably_url(&argument) {
        info!(url = %argument, "CLI аргумент распознан как YouTube/web URL");

        return match service_youtube::open_streaming_media(&argument) {
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
                (None, Some(format!("NetworkError: YouTube error: {error}")))
            }
        };
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    (Some(InitialMedia::File(PathBuf::from(argument))), None)
}

/// Открывает SQLite storage и возвращает user-facing startup error при сбое.
fn initialize_storage() -> (Option<StorageConnection>, Option<String>) {
    match rustiplayer_storage::open_or_create() {
        Ok(opened_storage) => {
            info!(
                path = %opened_storage.path.display(),
                schema_version = opened_storage.migration_report.current_version,
                applied_migrations = opened_storage.migration_report.applied_migrations.len(),
                "Storage rustiplayer готов"
            );
            (Some(opened_storage.connection), None)
        }
        Err(error) => {
            tracing::error!(error = %error, "Не удалось инициализировать storage rustiplayer");
            (None, Some(format!("StorageError: {error}")))
        }
    }
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

/// Объединяет startup-ошибки shell-слоя в одно UI-сообщение.
fn combine_startup_errors(errors: [Option<String>; 2]) -> Option<String> {
    let messages = errors.into_iter().flatten().collect::<Vec<_>>();

    if messages.is_empty() {
        None
    } else {
        Some(messages.join("\n"))
    }
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

        let error = player_error_from_render_failure(&failure);

        assert_eq!(error.kind, PlayerErrorKind::RenderDeviceLost);
        assert!(
            error
                .message
                .contains("P010 HDR renderer rejected strict metadata")
        );
    }
}
