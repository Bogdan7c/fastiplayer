//! Winit shell приложения.
//!
//! Модуль владеет lifecycle-слоем desktop shell:
//! - созданием окна через winit;
//! - восстановлением runtime-ресурсов после `resumed`;
//! - обработкой `WindowEvent`;
//! - координацией renderer, egui state и shell background jobs.
//!
//! Player/session/render internals остаются за своими модулями. Shell работает
//! через boundary methods `AppState`, `Renderer` и startup/redraw helpers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use capability_core::CapabilityScanner;
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrOutputMode, HdrToSdrSettings,
    HdrToneMappingOperator, RenderCapabilities, SwapchainTransferMode,
};
use render_wgpu_shell::Renderer;
use rustiplayer_config::{AppConfig, HdrToSdrOperatorConfig};
use tracing::{debug, info, instrument, warn};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow},
    window::Window,
};

use crate::frame_prepare::render_frame;
use crate::redraw_pacing::{RedrawPacing, should_request_redraw_after_window_event};
use crate::startup_media::{
    InitialMedia, YOUTUBE_STARTUP_POLL_INTERVAL, YoutubeStartupJob, poll_youtube_startup_job,
};
use crate::state::AppState;
use crate::telemetry::Telemetry;

/// Единый интервал polling-а shell background jobs, когда playback не даёт continuous redraw.
const BACKGROUND_JOB_POLL_INTERVAL: Duration = YOUTUBE_STARTUP_POLL_INTERVAL;

/// Winit shell приложения.
///
/// Владеет:
/// - окном (Arc<Window>)
/// - рендерером (`render-wgpu-shell` backend)
/// - состоянием приложения (egui + player state)
/// - телеметрией
/// - media для автозагрузки из CLI
pub(crate) struct AppShell {
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

    /// Ближайшее время, когда нужно снова проверить shell background jobs.
    next_background_job_poll_at: Option<Instant>,
}

impl AppShell {
    /// Создаёт пустой shell.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    pub(crate) fn new(
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
            next_background_job_poll_at: None,
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
        app_state.poll_local_file_open_job();
        self.refresh_background_job_poll_deadline(false);

        // Shell получает read-only snapshot без доступа к mutable playback internals.
        let _player_snapshot = app_state.player_snapshot();

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn drop_runtime(&mut self) {
        if let Some(app_state) = &mut self.app_state {
            app_state.clear_cached_present_frame_for_runtime_drop();

            if let Some(path) = app_state.current_local_file() {
                self.initial_media = Some(InitialMedia::File(path.to_path_buf()));
                self.startup_error = None;
            }
        }

        self.app_state = None;
        self.renderer = None;
    }

    /// Запускает background resolve для CLI YouTube URL и сразу обновляет UI state.
    fn start_youtube_startup_job(&mut self, source_url: String, app_state: &mut AppState) {
        app_state.set_startup_pending("Подготовка YouTube stream...".to_string());
        self.next_background_job_poll_at = None;
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
                self.next_background_job_poll_at = None;
            }
        }
    }

    /// Сбрасывает deadline polling-а, когда shell background jobs уже завершены.
    fn refresh_background_job_poll_deadline(&mut self, has_pending_local_file_open: bool) {
        if self.youtube_startup_job.is_none() && !has_pending_local_file_open {
            self.next_background_job_poll_at = None;
        }
    }

    /// Применяет pacing после render pass-а.
    fn apply_redraw_pacing(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Window,
        pacing: RedrawPacing,
    ) {
        if pacing.wants_continuous_redraw() {
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

    /// Настраивает idle ожидание: обычный Wait или timed wakeup для background jobs.
    fn configure_idle_control_flow(&mut self, event_loop: &ActiveEventLoop) {
        if !self.has_pending_background_job() {
            self.next_background_job_poll_at = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let deadline = self
            .next_background_job_poll_at
            .unwrap_or_else(|| Instant::now() + BACKGROUND_JOB_POLL_INTERVAL);
        self.next_background_job_poll_at = Some(deadline);
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }

    /// Возвращает `true`, если shell уже держит continuous redraw из-за playback.
    fn has_continuous_redraw(&self) -> bool {
        self.app_state
            .as_ref()
            .is_some_and(AppState::wants_continuous_redraw)
    }

    /// Возвращает `true`, если idle loop должен просыпаться для polling-а background jobs.
    fn has_pending_background_job(&self) -> bool {
        self.youtube_startup_job.is_some()
            || self
                .app_state
                .as_ref()
                .is_some_and(AppState::has_pending_local_file_open)
    }
}

impl ApplicationHandler for AppShell {
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
                app_state.poll_local_file_open_job();
                let pacing = render_frame(&self.telemetry, &window, renderer, app_state);
                let has_pending_local_file_open = app_state.has_pending_local_file_open();
                if self.youtube_startup_job.is_none() && !has_pending_local_file_open {
                    self.next_background_job_poll_at = None;
                }
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

        if !self.has_pending_background_job() {
            self.next_background_job_poll_at = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let now = Instant::now();
        let deadline = self.next_background_job_poll_at.unwrap_or(now);
        if now >= deadline {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            self.next_background_job_poll_at = Some(now + BACKGROUND_JOB_POLL_INTERVAL);
        }

        if let Some(next_deadline) = self.next_background_job_poll_at {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next_deadline));
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
}
