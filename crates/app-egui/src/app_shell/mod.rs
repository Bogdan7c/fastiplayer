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

use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakeProxy};
use crate::render_settings::{
    surface_present_settings_from_config, warn_legacy_tone_mapping_config,
};
use crate::system_capabilities::probe_system_capabilities;
use render_wgpu_shell::Renderer;
use rustiplayer_config::LoadedConfig;
use tracing::{debug, info, instrument};
use winit::{
    application::ApplicationHandler, dpi::PhysicalSize, event::WindowEvent,
    event_loop::ActiveEventLoop, window::Window,
};

use crate::frame_prepare::render_frame;
use crate::playlist_runtime::{PlaylistRuntime, PlaylistShutdownDeadline, PlaylistShutdownOutcome};
use crate::redraw_pacing::{
    BackgroundPollScheduler, RedrawControlAction, should_request_redraw_after_window_event,
};
use crate::renderer_recreation::RendererLifecycleCoordinator;
use crate::settings_runtime::SettingsRuntime;
use crate::startup_media::{InitialMedia, StartupMediaController};
use crate::state::AppState;
use crate::telemetry::Telemetry;

/// Применяет redraw только когда drain действительно изменил видимый state.
fn request_redraw_for_visible_wake(window: Option<&Window>, visible_mutation: bool) -> bool {
    let should_redraw = should_request_redraw_for_wake(window.is_some(), visible_mutation);
    if should_redraw && let Some(window) = window {
        window.request_redraw();
    }
    should_redraw
}

const fn should_request_redraw_for_wake(has_window: bool, visible_mutation: bool) -> bool {
    has_window && visible_mutation
}

/// Winit shell приложения.
///
/// Владеет:
/// - окном (`Arc<Window>`)
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

    /// Process-lifetime playlist owner переживает renderer-bound AppState recreation.
    playlist_runtime: PlaylistRuntime,

    /// Port копируется в каждый renderer-bound AppState при resume.
    local_file_open_wake_port: crate::app_wake::AppWakePort,

    /// Authoritative runtime owner пользовательских настроек.
    settings_runtime: SettingsRuntime,

    /// Scheduler idle wakeup-ов для shell background jobs.
    background_poll_scheduler: BackgroundPollScheduler,

    /// Сериализатор renderer recreation и surface resize lifecycle-а.
    renderer_lifecycle: RendererLifecycleCoordinator,
}

impl AppShell {
    /// Создаёт пустой shell.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    pub(crate) fn new(
        initial_media: Option<InitialMedia>,
        startup_error: Option<String>,
        loaded_config: LoadedConfig,
        wake_proxy: AppWakeProxy,
    ) -> anyhow::Result<Self> {
        let startup_wake_port = wake_proxy.port(AppWakeOwner::StartupMedia);
        let local_file_wake_port = wake_proxy.port(AppWakeOwner::LocalFileOpen);
        let settings_wake_port = wake_proxy.port(AppWakeOwner::SettingsDynamicOptions);
        let playlist_wake_port = wake_proxy.port(AppWakeOwner::PlaylistRuntime);
        Ok(Self {
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            startup_media: StartupMediaController::with_wake_port(
                initial_media,
                startup_error,
                startup_wake_port,
            ),
            playlist_runtime: PlaylistRuntime::new(playlist_wake_port),
            local_file_open_wake_port: local_file_wake_port,
            settings_runtime: SettingsRuntime::from_loaded_config_with_wake_port(
                loaded_config,
                settings_wake_port,
            )?,
            background_poll_scheduler: BackgroundPollScheduler::new(),
            renderer_lifecycle: RendererLifecycleCoordinator::default(),
        })
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

        let surface_present_settings =
            surface_present_settings_from_config(self.settings_runtime.committed_config());
        let mut renderer = match Renderer::new(window.clone(), surface_present_settings) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::error!("Не удалось инициализировать рендерер: {}", error);
                event_loop.exit();
                return;
            }
        };
        let initial_render_settings = match self.settings_runtime.initial_render_settings() {
            Ok(settings) => settings,
            Err(error) => {
                tracing::error!(error = %error, "Некорректные render color settings");
                event_loop.exit();
                return;
            }
        };
        warn_legacy_tone_mapping_config(self.settings_runtime.committed_config());
        renderer.set_color_pipeline_settings(initial_render_settings.color_pipeline);
        renderer.set_hdr_to_sdr_settings(initial_render_settings.hdr_to_sdr);

        let mut app_state = match AppState::new(
            &window,
            self.telemetry.clone(),
            self.settings_runtime.committed_snapshot(),
            self.settings_runtime.audio_output_device_controller(),
            self.startup_media.startup_error_message(),
            self.local_file_open_wake_port.clone(),
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
        app_state.set_system_capabilities(system_capabilities.clone());
        app_state.init_video_pipeline(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );

        self.startup_media.start_pending_initial_media(
            &mut app_state,
            self.settings_runtime.committed_config(),
            &system_capabilities,
        );
        self.startup_media.poll_startup_jobs(&mut app_state);
        app_state.poll_local_file_open_job();

        if self.playlist_runtime.bind_resumed_app_state().is_none() {
            tracing::error!("Playlist runtime уже закрыт и не принимает новый AppState binding");
            event_loop.exit();
            return;
        }
        self.playlist_runtime
            .attach_player_sender(app_state.player_command_sender());

        // Shell явно разделяет обновление snapshot и публикацию в desktop integration.
        let player_snapshot = app_state.refresh_player_snapshot();
        app_state.publish_desktop_snapshot(&player_snapshot);

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

        self.playlist_runtime.suspend_app_state_binding();
        self.app_state = None;
        self.renderer = None;
    }

    /// Закрывает приложение через единый cleanup path shell-а.
    fn close_runtime_and_exit(&mut self, event_loop: &ActiveEventLoop, reason: &'static str) {
        info!("{reason}");
        self.drop_runtime();
        self.shutdown_playlist_runtime();
        event_loop.exit();
    }

    /// Закрывает process-lifetime admission через единый idempotent boundary.
    fn shutdown_playlist_runtime(&mut self) {
        let shutdown_outcome = self
            .playlist_runtime
            .shutdown(PlaylistShutdownDeadline::at(Instant::now()));
        match shutdown_outcome {
            PlaylistShutdownOutcome::Completed => {
                info!("Playlist runtime bounded shutdown завершён");
            }
            PlaylistShutdownOutcome::AlreadyCompleted => {
                debug!("Playlist runtime уже был закрыт");
            }
        }
    }

    /// Применяет уже принятое scheduler-ом решение к winit и окну.
    fn apply_redraw_control_action(
        event_loop: &ActiveEventLoop,
        window: Option<&Window>,
        action: RedrawControlAction,
    ) {
        event_loop.set_control_flow(action.control_flow);

        if action.request_redraw
            && let Some(window) = window
        {
            window.request_redraw();
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
        self.startup_media.has_pending_startup_job()
            || self
                .app_state
                .as_ref()
                .is_some_and(AppState::has_pending_local_file_open)
            || self.settings_runtime.has_pending_options_refresh()
    }
}

impl ApplicationHandler<AppWakeEvent> for AppShell {
    /// Неблокирующе опустошает ровно одного owner-а и redraw-ит только mutation.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppWakeEvent) {
        let visible_mutation = match event.owner() {
            AppWakeOwner::StartupMedia => self
                .app_state
                .as_mut()
                .is_some_and(|app_state| self.startup_media.poll_startup_jobs(app_state)),
            AppWakeOwner::LocalFileOpen => {
                if let Some(app_state) = self.app_state.as_mut() {
                    app_state.poll_local_file_open_job()
                } else {
                    self.local_file_open_wake_port
                        .acknowledge_abandoned_mailbox();
                    false
                }
            }
            AppWakeOwner::SettingsDynamicOptions => {
                self.settings_runtime.poll_dynamic_options_refresh()
            }
            AppWakeOwner::PlaylistRuntime => {
                self.playlist_runtime.drain_owner_mailbox();
                false
            }
        };

        request_redraw_for_visible_wake(self.window.as_deref(), visible_mutation);
    }

    /// Гарантирует process-owner shutdown и для exit путей вне window callbacks.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.drop_runtime();
        self.shutdown_playlist_runtime();
    }

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
            .with_title("Rustiplayer")
            .with_inner_size(winit::dpi::PhysicalSize::new(1280, 720))
            .with_decorations(false)
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
        app_state.sync_committed_config_snapshot(self.settings_runtime.committed_snapshot());

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
                self.close_runtime_and_exit(event_loop, "Закрытие окна по запросу пользователя");
                return;
            }

            WindowEvent::Resized(PhysicalSize { width, height }) => {
                debug!(width, height, "Изменение размера окна");
                self.renderer_lifecycle
                    .resize_renderer(renderer, width, height);
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
                    self.close_runtime_and_exit(event_loop, "Выход по Escape");
                    return;
                }
                other => {
                    app_state.handle_hotkeys(&window, other, egui_response.consumed);
                }
            },

            WindowEvent::RedrawRequested => {
                self.startup_media.poll_startup_jobs(app_state);
                app_state.poll_local_file_open_job();
                let frame_result = render_frame(
                    &self.telemetry,
                    &window,
                    renderer,
                    app_state,
                    &mut self.settings_runtime,
                    &mut self.renderer_lifecycle,
                );
                if frame_result.close_requested {
                    self.close_runtime_and_exit(
                        event_loop,
                        "Закрытие окна через кастомный titlebar",
                    );
                    return;
                }
                let has_pending_background_job = self.startup_media.has_pending_startup_job()
                    || app_state.has_pending_local_file_open()
                    || self.settings_runtime.has_pending_options_refresh();
                let action = self.background_poll_scheduler.after_render(
                    frame_result.pacing,
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

#[cfg(test)]
mod tests {
    use super::should_request_redraw_for_wake;

    #[test]
    fn idle_and_noop_wakes_never_request_redraw() {
        assert!(!should_request_redraw_for_wake(true, false));
        assert!(!should_request_redraw_for_wake(false, false));
    }

    #[test]
    fn visible_wake_requires_live_window() {
        assert!(should_request_redraw_for_wake(true, true));
        assert!(!should_request_redraw_for_wake(false, true));
    }
}
