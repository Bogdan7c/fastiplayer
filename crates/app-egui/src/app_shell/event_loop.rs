//! Политика событий winit для process-lifetime [`AppShell`].
//!
//! Этот приватный модуль переводит callbacks `ApplicationHandler` в intent-методы shell-а.
//! Сам [`AppShell`] по-прежнему владеет окном, renderer lease, runtime binding и порядком
//! suspend/resume/shutdown; здесь нет второго lifecycle state или скрытого resource owner-а.

use std::{sync::Arc, time::Instant};

use tracing::{debug, info, instrument};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    platform::external_drag::ExternalDragEvent,
    window::{Window, WindowId},
};

use crate::app_wake::{AppWakeEvent, AppWakeOwner};
use crate::external_open::winit_source::gesture_event_from_external_drag;
use crate::fatal_startup::FatalStartupError;
use crate::frame_prepare::render_frame;
use crate::redraw_pacing::{
    RedrawCause, RedrawControlAction, should_request_redraw_after_window_event,
};
use crate::state::AppState;

use super::AppShell;
use super::escape_dismissal;
use super::hotkeys::{self, ShellHotkeyAction};
use super::shutdown::AppShellProcessLifecycle;
use crate::app_instance::RelaunchRequest;
use crate::ui::keyboard_focus::keyboard_focus_owner;

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

/// Страховочный redraw воспроизведения помечается в `AppState`: кадр посчитает, нашла ли
/// страховка работу (признак пропущенного события), а остальные причины redraw ничего
/// не помечают — их кадр узнаёт сам (ввод, wake, дедлайны).
fn note_scheduled_redraw_cause(app_state: &mut AppState, action: RedrawControlAction) {
    if action.redraw_cause == Some(RedrawCause::PlaybackFallback) {
        app_state.note_redraw_fallback_probe();
    }
}

/// Видимая мутация от owner-wake обязана дать полный egui-проход на следующем кадре
/// (сессия 04): иначе кадр мог бы решить, что интерфейс не менялся, и пропустить её.
fn note_visible_mutation_for_redraw(app_state: Option<&mut AppState>) {
    if let Some(app_state) = app_state {
        app_state.note_visible_mutation_for_redraw();
    }
}

impl ApplicationHandler<AppWakeEvent> for AppShell {
    /// Неблокирующе опустошает ровно одного owner-а и redraw-ит только mutation.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppWakeEvent) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let visible_mutation = match event.owner() {
            AppWakeOwner::StartupMedia => {
                match (self.app_state.as_mut(), self.renderer.as_deref_mut()) {
                    (Some(app_state), Some(renderer)) => self.startup_media.poll_startup_jobs(
                        app_state,
                        &mut self.playlist_runtime,
                        renderer,
                    ),
                    _ => false,
                }
            }
            AppWakeOwner::LocalFileOpen => {
                match (self.app_state.as_mut(), self.renderer.as_deref_mut()) {
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
            AppWakeOwner::InstanceForwarding => self.drain_instance_forwarding(),
            AppWakeOwner::Subtitles => match self.app_state.as_mut() {
                Some(app_state) => app_state.poll_subtitle_jobs(),
                None => {
                    self.subtitle_wake_port.acknowledge_abandoned_mailbox();
                    false
                }
            },
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
            // Сессия 04: worker опубликовал кадр показа или событие плеера. Порт живёт в
            // `AppState`; без него (suspend) событие просто теряется вместе с прежним edge-ом.
            AppWakeOwner::PlayerPresentation => match self.app_state.as_mut() {
                Some(app_state) => {
                    app_state.note_presentation_wake();
                    true
                }
                None => false,
            },
        };

        // Wake показа — не «видимая мутация интерфейса»: что рисовать (кадр, интерфейс или
        // ничего), решает сам кадр по слоту показа и событиям. Пометка здесь заставила бы
        // делать полный egui-проход на каждый новый кадр.
        if visible_mutation && event.owner() != AppWakeOwner::PlayerPresentation {
            note_visible_mutation_for_redraw(self.app_state.as_mut());
        }
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
        info!("Suspending: releasing runtime resources");
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

        info!("Resumed: creating window");

        let window_attributes = super::main_window_attributes::main_window_attributes();

        let window = match event_loop.create_window(window_attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.exit_with_fatal_startup_error(
                    event_loop,
                    FatalStartupError::window_creation_failed(&error),
                );
                return;
            }
        };

        info!(
            width = window.inner_size().width,
            height = window.inner_size().height,
            scale_factor = window.scale_factor(),
            "Window created"
        );

        self.window = Some(window.clone());
        self.restore_runtime(event_loop, window);
        let post_resume_changed = self.drain_playlist_persistence();
        request_redraw_for_visible_wake(
            self.window.as_deref(),
            persistence_changed || post_resume_changed,
        );
    }

    /// Принимает файлы и ссылки, перетаскиваемые в окно (канал патча winit, Linux).
    ///
    /// Перевод события и вся логика броска живут в `external_open`; shell только
    /// пересылает событие владельцу и просит перерисовку.
    fn external_drag_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: ExternalDragEvent,
    ) {
        if let Some(gesture_event) = gesture_event_from_external_drag(event) {
            self.handle_external_drop_gesture(gesture_event);
        }
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
        // Любое оконное событие, кроме самого redraw и закрытия, до сессии 04 давало redraw с
        // полным проходом; теперь оно помечает «был ввод», и следующий кадр обязан сделать
        // egui-проход (ввод не теряется: egui уже получил событие выше).
        if redraw_after_event {
            app_state.note_window_input_for_redraw();
        }

        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
        {
            // `egui_response.consumed` для клавиш здесь не используется: egui-winit
            // ставит его по `egui_wants_keyboard_input()`, то есть при фокусе на
            // любой кнопке, и хоткеи глохли бы после клика мышью. Какие клавиши
            // отдать виджету, решает политика владельца клавиатуры.
            let keyboard_owner = keyboard_focus_owner(&app_state.egui_ctx);
            if let Some(action) = hotkeys::classify_key_event(key_event, keyboard_owner) {
                match action {
                    ShellHotkeyAction::DismissTopmost => {
                        let _dismissed = escape_dismissal::dismiss_topmost(
                            &window,
                            app_state,
                            &mut self.playlist_runtime,
                            renderer.as_mut(),
                        );
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
                            renderer.as_mut(),
                            &snapshot,
                            action,
                        );
                    }
                }
                window.request_redraw();
                return;
            }
        }

        // Вне Linux файлы приходят устаревшими событиями winit (без позиции): собираем их в
        // жест. На Linux эти же события дублируют канал `external_drag` и не используются.
        #[cfg(not(target_os = "linux"))]
        if let Some(gesture_event) = app_state.ingest_legacy_file_drop_event(&event) {
            app_state.handle_external_drop_gesture(
                gesture_event,
                &mut self.playlist_runtime,
                renderer.as_mut(),
            );
            window.request_redraw();
        }

        if egui_response.consumed {
            if redraw_after_event {
                window.request_redraw();
            }
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                self.close_runtime_and_exit(event_loop, "Closing window at user request");
                return;
            }

            WindowEvent::Resized(PhysicalSize { width, height }) => {
                debug!(width, height, "Window resized");
                self.renderer_lifecycle
                    .resize_renderer(renderer.as_mut(), width, height);
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                super::scale_factor::on_scale_factor_changed(&app_state.egui_ctx, scale_factor);
            }

            WindowEvent::RedrawRequested => {
                self.startup_media.poll_startup_jobs(
                    app_state,
                    &mut self.playlist_runtime,
                    renderer.as_mut(),
                );
                app_state.poll_local_file_open_job(&mut self.playlist_runtime, renderer.as_mut());
                let frame_result = render_frame(
                    &self.telemetry,
                    &window,
                    renderer,
                    app_state,
                    &mut self.playlist_runtime,
                    &mut self.settings_runtime,
                    &mut self.renderer_lifecycle,
                );
                // Первый показанный кадр любого вида завершает старт рендера (снимает метку).
                self.renderer_startup
                    .settle_after_frame(|| self.telemetry.frames_presented_to_surface());
                if frame_result.close_requested {
                    self.close_runtime_and_exit(
                        event_loop,
                        "Closing window through custom titlebar",
                    );
                    return;
                }
                // «Restart now» из настроек: тот же штатный выход, что и закрытие окна; сам
                // запуск нового процесса делает `main` уже после освобождения замка.
                if self.settings_runtime.relaunch_request() == RelaunchRequest::AfterExit {
                    self.close_runtime_and_exit(
                        event_loop,
                        "Closing window to restart the application",
                    );
                    return;
                }
                let has_pending_background_job = self.startup_media.has_pending_startup_job()
                    || app_state.has_pending_prepared_media_strong()
                    || app_state.has_pending_playlist_transport()
                    || self.playlist_runtime.has_pending_media_reset()
                    || app_state.has_pending_local_file_open()
                    || self.settings_runtime.has_pending_options_refresh()
                    || self.settings_runtime.has_pending_apply()
                    || self
                        .playlist_runtime
                        .has_pending_playlist_persistence_work();
                let action = self.background_poll_scheduler.after_render_with_deadline(
                    frame_result.pacing,
                    has_pending_background_job,
                    frame_result.next_ui_wake_deadline,
                    Instant::now(),
                );
                note_scheduled_redraw_cause(app_state, action);
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
        // Бросок устаревшего источника завершается раз за проход цикла, после всех DroppedFile.
        #[cfg(not(target_os = "linux"))]
        self.flush_legacy_file_drop();
        let persistence_changed = self.drain_playlist_persistence();
        if persistence_changed {
            note_visible_mutation_for_redraw(self.app_state.as_mut());
        }
        request_redraw_for_visible_wake(self.window.as_deref(), persistence_changed);
        let playback_active = self.has_continuous_redraw();
        let has_pending_background_job = self.has_pending_background_job();
        let now = Instant::now();
        let action = self.background_poll_scheduler.before_idle_wait(
            playback_active,
            has_pending_background_job,
            now,
        );
        if let Some(app_state) = self.app_state.as_mut() {
            note_scheduled_redraw_cause(app_state, action);
        }

        Self::apply_redraw_control_action(event_loop, self.window.as_deref(), action);
    }
}
