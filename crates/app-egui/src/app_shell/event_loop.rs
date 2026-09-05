//! Политика событий winit для process-lifetime [`AppShell`].
//!
//! Этот приватный модуль переводит callbacks `ApplicationHandler` в intent-методы shell-а.
//! Сам [`AppShell`] по-прежнему владеет окном, renderer lease, runtime binding и порядком
//! suspend/resume/shutdown; здесь нет второго lifecycle state или скрытого resource owner-а.

use std::{sync::Arc, time::Instant};

use tracing::{debug, info, instrument};
use winit::{
    application::ApplicationHandler, dpi::PhysicalSize, event::WindowEvent,
    event_loop::ActiveEventLoop, window::Window,
};

use crate::app_wake::{AppWakeEvent, AppWakeOwner};
use crate::frame_prepare::render_frame;
use crate::redraw_pacing::should_request_redraw_after_window_event;

use super::AppShell;
use super::hotkeys::{self, ShellHotkeyAction};
use super::shutdown::AppShellProcessLifecycle;

/// Запрашивает redraw только после реально видимой UI mutation и только для живого окна.
fn request_redraw_for_visible_wake(window: Option<&Window>, visible_mutation: bool) -> bool {
    let should_redraw = should_request_redraw_for_wake(window.is_some(), visible_mutation);
    if should_redraw && let Some(window) = window {
        window.request_redraw();
    }
    should_redraw
}

pub(super) const fn should_request_redraw_for_wake(
    has_window: bool,
    visible_mutation: bool,
) -> bool {
    has_window && visible_mutation
}

impl ApplicationHandler<AppWakeEvent> for AppShell {
    /// Неблокирующе опустошает ровно одного owner-а и redraw-ит только mutation.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppWakeEvent) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let visible_mutation = match event.owner() {
            AppWakeOwner::StartupMedia => match (self.app_state.as_mut(), self.renderer.as_ref()) {
                (Some(app_state), Some(renderer)) => self.startup_media.poll_startup_jobs(
                    app_state,
                    &mut self.playlist_runtime,
                    renderer,
                ),
                _ => false,
            },
            AppWakeOwner::LocalFileOpen => {
                match (self.app_state.as_mut(), self.renderer.as_ref()) {
                    (Some(app_state), Some(renderer)) => {
                        app_state.poll_local_file_open_job(&mut self.playlist_runtime, renderer)
                    }
                    _ => {
                        self.local_file_open_wake_port
                            .acknowledge_abandoned_mailbox();
                        false
                    }
                }
            }
            AppWakeOwner::SettingsDynamicOptions => {
                self.settings_runtime.poll_dynamic_options_refresh()
            }
            AppWakeOwner::PlaylistRuntime => self.drain_playlist_persistence(),
            AppWakeOwner::PlayerTimeline => {
                self.player_timeline_wake_port.clear_pending_for_drain();
                match self.app_state.as_mut() {
                    Some(app_state) => {
                        match app_state.refresh_player_snapshot_if_timeline_changed() {
                            Some(player_snapshot) => {
                                self.playlist_runtime
                                    .publish_desktop_snapshot(&player_snapshot);
                                true
                            }
                            None => false,
                        }
                    }
                    None => false,
                }
            }
        };

        request_redraw_for_visible_wake(self.window.as_deref(), visible_mutation);
    }

    /// Гарантирует process-owner shutdown и для exit путей вне window callbacks.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.finish_process_shutdown();
    }

    /// Вызывается при приостановке приложения (сворачивание, смена TTY).
    ///
    /// Освобождаем GPU ресурсы — surface может стать невалидным.
    #[instrument(skip(self))]
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        info!("Приостановка: освобождаем runtime-ресурсы");
        self.suspend_runtime();
    }

    /// Вызывается при возобновлении работы (разворачивание, первый запуск).
    ///
    /// Здесь создаём окно, инициализируем wgpu и egui.
    #[instrument(skip(self, event_loop))]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let persistence_changed = self.drain_playlist_persistence();
        if let Some(window) = self.window.clone() {
            self.restore_runtime(event_loop, window);
            let post_resume_changed = self.drain_playlist_persistence();
            request_redraw_for_visible_wake(
                self.window.as_deref(),
                persistence_changed || post_resume_changed,
            );
            return;
        }

        info!("Resumed: создание окна");

        let window_attributes = Window::default_attributes()
            .with_title("Fastiplayer")
            .with_inner_size(winit::dpi::PhysicalSize::new(1280, 720))
            // Ширина гарантирует полный порядок transport controls; единичная высота
            // оставляет вертикальное ограничение практически на усмотрение compositor-а.
            .with_min_inner_size(winit::dpi::LogicalSize::new(400.0, 1.0))
            .with_decorations(false)
            // X11 требует запросить alpha-capable native window до создания WGPU surface.
            .with_transparent(true)
            .with_visible(true);

        let window = match event_loop.create_window(window_attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                tracing::error!("Не удалось создать окно: {}", error);
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
        let post_resume_changed = self.drain_playlist_persistence();
        request_redraw_for_visible_wake(
            self.window.as_deref(),
            persistence_changed || post_resume_changed,
        );
    }

    /// Обрабатывает ввод, ресайз, закрытие и redraw активного окна.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let (Some(renderer), Some(app_state)) = (&mut self.renderer, &mut self.app_state) else {
            return;
        };
        app_state.sync_committed_config_snapshot(self.settings_runtime.committed_snapshot());

        // Сначала отдаём событие egui, чтобы shell hotkeys не перехватывали текстовый ввод.
        let egui_response = app_state.egui_winit_state.on_window_event(&window, &event);
        let redraw_after_event = should_request_redraw_after_window_event(&event);

        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
        {
            let keyboard_captured = egui_response.consumed
                || app_state.egui_ctx.egui_wants_keyboard_input()
                || app_state.egui_ctx.text_edit_focused();
            if let Some(action) = hotkeys::classify_key_event(key_event, keyboard_captured) {
                match action {
                    ShellHotkeyAction::Close => {
                        self.close_runtime_and_exit(event_loop, "Выход по Escape");
                        return;
                    }
                    ShellHotkeyAction::Legacy(key_code) => {
                        app_state.handle_hotkeys(&window, key_code, false);
                    }
                    ShellHotkeyAction::Transport(action) => {
                        let snapshot = app_state.refresh_player_snapshot();
                        self.playlist_runtime.publish_desktop_snapshot(&snapshot);
                        crate::transport_runtime::apply_transport_action(
                            app_state,
                            &mut self.playlist_runtime,
                            renderer,
                            &snapshot,
                            action,
                        );
                    }
                }
                window.request_redraw();
                return;
            }
        }

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

            WindowEvent::RedrawRequested => {
                self.startup_media.poll_startup_jobs(
                    app_state,
                    &mut self.playlist_runtime,
                    renderer,
                );
                app_state.poll_local_file_open_job(&mut self.playlist_runtime, renderer);
                let frame_result = render_frame(
                    &self.telemetry,
                    &window,
                    renderer,
                    app_state,
                    &mut self.playlist_runtime,
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
                    || app_state.has_pending_prepared_media_strong()
                    || app_state.has_pending_playlist_transport()
                    || self.playlist_runtime.has_pending_media_reset()
                    || app_state.has_pending_local_file_open()
                    || self.settings_runtime.has_pending_options_refresh()
                    || self
                        .playlist_runtime
                        .has_pending_playlist_persistence_work();
                let action = self.background_poll_scheduler.after_render_with_deadline(
                    frame_result.pacing,
                    has_pending_background_job,
                    frame_result.next_ui_wake_deadline,
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
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let persistence_changed = self.drain_playlist_persistence();
        request_redraw_for_visible_wake(self.window.as_deref(), persistence_changed);
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
