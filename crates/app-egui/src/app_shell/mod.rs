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
use std::time::Instant;

use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
    warn_legacy_tone_mapping_config,
};
use crate::system_capabilities::probe_system_capabilities;
use render_wgpu_shell::Renderer;
use rustiplayer_config::AppConfig;
use tracing::{debug, info, instrument};
use winit::{
    application::ApplicationHandler, dpi::PhysicalSize, event::WindowEvent,
    event_loop::ActiveEventLoop, window::Window,
};

use crate::frame_prepare::render_frame;
use crate::redraw_pacing::{
    BackgroundPollScheduler, RedrawControlAction, should_request_redraw_after_window_event,
};
use crate::startup_media::{InitialMedia, StartupMediaController};
use crate::state::AppState;
use crate::telemetry::Telemetry;

/// Winit shell приложения.
///
/// Владеет:
/// - окном (Arc<Window>)
/// - рендерером (`render-wgpu-shell` backend)
/// - состоянием приложения (egui + player state)
/// - телеметрией
/// - controller-ом стартового media lifecycle
pub(crate) struct AppShell {
    /// Окно приложения. None до Resumed.
    window: Option<Arc<Window>>,

    /// Рендерер с GPU ресурсами. None до Resumed.
    renderer: Option<Renderer>,

    /// Состояние приложения (egui, player state). None до Resumed.
    app_state: Option<AppState>,

    /// Общая телеметрия.
    telemetry: Arc<Telemetry>,

    /// Controller стартового media и фоновой подготовки CLI YouTube URL.
    startup_media: StartupMediaController,

    /// Валидированная пользовательская конфигурация.
    app_config: AppConfig,

    /// Scheduler idle wakeup-ов для shell background jobs.
    background_poll_scheduler: BackgroundPollScheduler,
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
            startup_media: StartupMediaController::new(initial_media, startup_error),
            app_config,
            background_poll_scheduler: BackgroundPollScheduler::new(),
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
            self.startup_media.startup_error_message(),
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

        self.startup_media
            .start_pending_initial_media(&mut app_state, &self.app_config);
        self.startup_media.poll_youtube_job(&mut app_state);
        app_state.poll_local_file_open_job();

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
                self.startup_media.restore_file_on_next_resume(path);
            }
        }

        self.app_state = None;
        self.renderer = None;
    }

    /// Применяет уже принятое scheduler-ом решение к winit и окну.
    fn apply_redraw_control_action(
        event_loop: &ActiveEventLoop,
        window: Option<&Window>,
        action: RedrawControlAction,
    ) {
        event_loop.set_control_flow(action.control_flow);

        if action.request_redraw {
            if let Some(window) = window {
                window.request_redraw();
            }
        }
    }

    /// Возвращает `true`, если shell уже держит continuous redraw из-за playback.
    fn has_continuous_redraw(&self) -> bool {
        self.app_state
            .as_ref()
            .is_some_and(AppState::wants_continuous_redraw)
    }

    /// Возвращает `true`, если idle loop должен просыпаться для polling-а background jobs.
    fn has_pending_background_job(&self) -> bool {
        self.startup_media.has_pending_youtube_job()
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
            .with_title("rustiplayer")
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
                self.startup_media.poll_youtube_job(app_state);
                app_state.poll_local_file_open_job();
                let pacing = render_frame(&self.telemetry, &window, renderer, app_state);
                let has_pending_background_job = self.startup_media.has_pending_youtube_job()
                    || app_state.has_pending_local_file_open();
                let action = self.background_poll_scheduler.after_render(
                    pacing,
                    has_pending_background_job,
                    Instant::now(),
                );
                Self::apply_redraw_control_action(event_loop, Some(&window), action);
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
        let has_continuous_redraw = self.has_continuous_redraw();
        let has_pending_background_job = self.has_pending_background_job();
        let now = Instant::now();
        let action = self.background_poll_scheduler.before_idle_wait(
            has_continuous_redraw,
            has_pending_background_job,
            now,
        );

        Self::apply_redraw_control_action(event_loop, self.window.as_deref(), action);
    }
}
