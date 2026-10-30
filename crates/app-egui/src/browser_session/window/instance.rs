//! Экземпляр окна браузерного режима: окно + рендер + `AppState` + настройки + очередь.
//!
//! Решение 121: один экземпляр окна = один источник. В 04a экземпляр один, но ничего
//! здесь не считает себя единственным в процессе: всё, чем владеет экземпляр, — его
//! собственное (окно, рендер, плеер, настройки «на сеанс», пустая очередь в памяти,
//! память дорожек в памяти); общее процессу (ядро сеанса, водитель журнала) живёт
//! снаружи и приходит параметрами. Вид заголовка и ресайз — будущий параметр
//! экземпляра («Наклейка», 17–19); в 04a окно — как у обычного плеера.
//!
//! Сборка повторяет шаги обычного окна (`AppShell::restore_runtime`) теми же деталями:
//! выбор рендера по `render.profile`, `AppState::new`, возможности системы, конвейер
//! видео, привязка очереди. Чего нет и почему:
//! - очередь без хранилищ и без MPRIS (`PlaylistRuntime::new_memory_only`);
//! - память дорожек только в оперативке;
//! - метка старта рендера только читается (`ReadOnlyRendererStartupMarker`);
//! - нет стартового медиа из командной строки, пересылки второго запуска, suspend-
//!   возобновления — у браузерного режима их не бывает.

use std::sync::Arc;
use std::time::Instant;

use render_shell_api::RenderShell;
use tracing::{debug, info, warn};
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use super::super::playback::metered_audio::{AudioMeter, BrowserAudioOutputChoice};
use super::driver::WindowPlaybackDriver;
use super::player_port::AppStateWindowPlayer;
use crate::app_shell::{
    ReadOnlyRendererStartupMarker, ShellKeyOutcome, handle_shell_key_event, main_window_attributes,
};
use crate::app_wake::{AppWakeOwner, AppWakePort, AppWakeProxy};
use crate::frame_prepare::render_frame;
use crate::playlist_runtime::PlaylistRuntime;
use crate::process_shutdown::ShutdownDeadline;
use crate::redraw_pacing::{
    BackgroundPollScheduler, RedrawCause, RedrawControlAction,
    should_request_redraw_after_window_event,
};
use crate::render_backend::{StartedRenderer, create_startup_renderer};
use crate::renderer_recreation::RendererLifecycleCoordinator;
use crate::settings_runtime::{HardwareDecodeAvailability, SettingsRuntime};
use crate::state::{
    AppState, AppStateStartupContext, RendererVideoImportTarget, start_track_memory_in_memory_only,
};
use crate::system_capabilities::probe_system_capabilities;
use crate::telemetry::Telemetry;
use crate::viewer_transport::ViewerTransportIntent;

/// Почему окно браузерного режима не собралось.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BrowserWindowStartError {
    /// Цикл событий не создался (нет графического сеанса).
    #[error("no graphical session for the browser window: {0}")]
    EventLoop(String),
    /// Настройки окна не собрались.
    #[error("browser window settings: {0}")]
    Settings(String),
    /// Окно не создалось.
    #[error("the browser window could not be created: {0}")]
    Window(String),
    /// Ни один рендер не поднялся.
    #[error("no renderer for the browser window: {0}")]
    Renderer(String),
    /// Плеер окна не поднялся.
    #[error("the browser window player did not start: {0}")]
    AppState(String),
    /// Очередь окна не собралась или не приняла плеер.
    #[error("the browser window queue did not start: {0}")]
    Playlist(String),
}

/// Порты пробуждения владельцев экземпляра (над общим циклом процесса).
pub(super) struct InstanceWakePorts {
    settings_dynamic_options: AppWakePort,
    playlist_runtime: AppWakePort,
    player_timeline: AppWakePort,
    subtitles: AppWakePort,
    local_file_open: AppWakePort,
}

impl InstanceWakePorts {
    /// Порты всех владельцев экземпляра. В 04a экземпляр один, и порты по владельцу
    /// однозначны; несколько экземпляров (19) потребуют различать их в событии.
    pub(super) fn from_proxy(proxy: &AppWakeProxy) -> Self {
        Self {
            settings_dynamic_options: proxy.port(AppWakeOwner::SettingsDynamicOptions),
            playlist_runtime: proxy.port(AppWakeOwner::PlaylistRuntime),
            player_timeline: proxy.port(AppWakeOwner::PlayerTimeline),
            subtitles: proxy.port(AppWakeOwner::Subtitles),
            local_file_open: proxy.port(AppWakeOwner::LocalFileOpen),
        }
    }
}

/// Куда идёт звук экземпляра.
pub(super) struct InstanceAudio {
    /// Настоящее устройство или тихая карта тестов (`FASTIPLAYER_TEST_BROWSER_AUDIO`).
    pub(super) choice: BrowserAudioOutputChoice,
    /// Счётчик журнала (общий с водителем окна).
    pub(super) meter: AudioMeter,
}

/// Что экземпляр просит у цикла после оконного события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstanceSignal {
    /// Ничего особенного.
    Continue,
    /// Человек закрыл окно (крестик, заголовок, Alt+F4).
    CloseRequested,
}

/// Экземпляр окна браузерного режима.
pub(super) struct BrowserWindowInstance {
    window: Arc<Window>,
    renderer: Box<dyn RenderShell>,
    app_state: AppState,
    settings_runtime: SettingsRuntime,
    playlist_runtime: PlaylistRuntime,
    renderer_lifecycle: RendererLifecycleCoordinator,
    telemetry: Arc<Telemetry>,
    scheduler: BackgroundPollScheduler,
    player_timeline_wake_port: AppWakePort,
    local_file_open_wake_port: AppWakePort,
    subtitle_wake_port: AppWakePort,
}

impl BrowserWindowInstance {
    /// Создаёт окно и всё, что за ним стоит. Окно сразу просит фокус.
    pub(super) fn create(
        event_loop: &ActiveEventLoop,
        loaded_config: fastiplayer_config::LoadedConfig,
        ports: InstanceWakePorts,
        audio: InstanceAudio,
    ) -> Result<Self, BrowserWindowStartError> {
        let started_at = Instant::now();
        let mut settings_runtime = SettingsRuntime::from_loaded_config_with_wake_port(
            loaded_config,
            ports.settings_dynamic_options,
        )
        .map_err(|error| BrowserWindowStartError::Settings(error.to_string()))?;
        let playlist_runtime = PlaylistRuntime::new_memory_only(
            ports.playlist_runtime,
            settings_runtime.committed_config().playlist,
        )
        .map_err(|error| BrowserWindowStartError::Playlist(format!("{error:?}")))?;

        let window = event_loop
            .create_window(main_window_attributes())
            .map(Arc::new)
            .map_err(|error| BrowserWindowStartError::Window(error.to_string()))?;
        // Окно появляется по просьбе страницы — оно должно оказаться перед человеком.
        window.focus_window();

        let initial_render_settings = settings_runtime
            .initial_render_settings()
            .map_err(|error| BrowserWindowStartError::Settings(error.to_string()))?;
        let mut marker = renderer_startup_marker();
        let StartedRenderer {
            mut renderer,
            report,
        } = create_startup_renderer(
            settings_runtime.committed_config(),
            window.clone(),
            initial_render_settings,
            &mut marker,
        )
        .map_err(|error| BrowserWindowStartError::Renderer(error.to_string()))?;
        settings_runtime.set_active_renderer_family(report.family());

        // Звук — на устройство из настроек окна (или тихая карта тестов), со счётчиком
        // журнала: выбор устройства в окне настроек доходит до него как в обычном окне.
        let audio_output = audio.choice.metered_factory_for(
            settings_runtime.audio_output_device_controller(),
            audio.meter,
        );
        let telemetry = Arc::new(Telemetry::new());
        let track_memory = start_track_memory_in_memory_only(track_memory_capacity(
            settings_runtime
                .committed_config()
                .playlist
                .track_memory_max_entries,
        ));
        // Семейство рендера известно: `AppState` по нему выбирает «авто»-пул поверхностей
        // аппаратного декодера (`hardware_surface_pool_default_or_wgpu`).
        let startup_context = AppStateStartupContext::new(
            started_at,
            None,
            None,
            ports.subtitles.clone(),
            track_memory,
        )
        .with_renderer_report(report)
        .with_audio_output_factory(audio_output)
        .with_player_event_tap()
        // Вид без очереди и открытия, транспорт ведёт страница (решение 46, 04a).
        .for_browser_session_window();
        let mut app_state = AppState::new(
            &window,
            startup_context,
            telemetry.clone(),
            settings_runtime.committed_snapshot(),
            settings_runtime.audio_output_device_controller(),
            ports.local_file_open.clone(),
            ports.player_timeline.clone(),
        )
        .map_err(|error| BrowserWindowStartError::AppState(format!("{error:#}")))?;

        let system_capabilities = probe_system_capabilities(renderer.capabilities().clone());
        info!("{}", system_capabilities.summary_text());
        settings_runtime.set_hardware_decode_availability(
            HardwareDecodeAvailability::from_system_capabilities(&system_capabilities),
        );
        app_state.set_system_capabilities(system_capabilities);
        app_state.init_video_pipeline(RendererVideoImportTarget::for_renderer(renderer.as_mut()));

        let mut instance = Self {
            window,
            renderer,
            app_state,
            settings_runtime,
            playlist_runtime,
            renderer_lifecycle: RendererLifecycleCoordinator::default(),
            telemetry,
            scheduler: BackgroundPollScheduler::new(),
            player_timeline_wake_port: ports.player_timeline,
            local_file_open_wake_port: ports.local_file_open,
            subtitle_wake_port: ports.subtitles,
        };
        instance.bind_queue_and_audio_level()?;
        info!(
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1_000.0,
            "Browser window ready"
        );
        instance.window.request_redraw();
        Ok(instance)
    }

    /// Привязка пустой очереди к плееру и уровень громкости пользователя.
    fn bind_queue_and_audio_level(&mut self) -> Result<(), BrowserWindowStartError> {
        let binding = self
            .playlist_runtime
            .bind_resumed_app_state()
            .ok_or_else(|| {
                BrowserWindowStartError::Playlist("the queue does not accept a player".to_string())
            })?;
        let attachment = self
            .playlist_runtime
            .app_state_attachment(binding)
            .map_err(|error| BrowserWindowStartError::Playlist(format!("{error:?}")))?;
        self.app_state.attach_playlist_runtime(attachment);
        self.playlist_runtime
            .attach_player_sender(self.app_state.player_command_sender());
        let level = self
            .settings_runtime
            .begin_player_audio_level_restore(Instant::now());
        if let Err(error) = crate::user_audio_level::send_user_audio_level_to_player(
            &self.app_state.player_command_sender(),
            level,
        ) {
            warn!(%error, "The browser window player did not accept the user audio level");
        }
        Ok(())
    }

    /// Каждый проход цикла (как `about_to_wait` обычного окна): ящики очереди, в том
    /// числе квитанции установки медиа от плеера. Квитанция плеера не будит цикл сама —
    /// координатор открытия продвигается только этим опросом.
    pub(super) fn drain_queue_each_pass(&mut self) {
        if self.drain_queue_owners() {
            self.app_state.note_visible_mutation_for_redraw();
            self.window.request_redraw();
        }
    }

    /// Действия зрителя с транспортом с прошлого прохода (кнопки, таймлайн, хоткеи) —
    /// для медиа-части сеанса: она ведёт и плеер, и страницу.
    pub(super) fn take_viewer_transport_intents(&mut self) -> Vec<ViewerTransportIntent> {
        self.app_state.take_viewer_transport_intents()
    }

    /// Плеер окна для водителя на время одного прохода.
    pub(super) fn player(&mut self) -> AppStateWindowPlayer<'_> {
        AppStateWindowPlayer {
            app_state: &mut self.app_state,
            playlist_runtime: &mut self.playlist_runtime,
            renderer: self.renderer.as_mut(),
            telemetry: &self.telemetry,
        }
    }

    /// Пробуждение владельца экземпляра (события плеера, настройки, субтитры, очередь).
    pub(super) fn on_owner_wake(&mut self, owner: AppWakeOwner) {
        let visible_mutation = match owner {
            AppWakeOwner::PlayerPresentation => {
                // Кадр показа или событие плеера: что рисовать, решает сам кадр.
                self.app_state.note_presentation_wake();
                self.window.request_redraw();
                return;
            }
            AppWakeOwner::PlayerTimeline => {
                self.player_timeline_wake_port.clear_pending_for_drain();
                self.app_state
                    .refresh_player_snapshot_if_timeline_changed()
                    .is_some()
            }
            AppWakeOwner::SettingsDynamicOptions => {
                self.settings_runtime.poll_dynamic_options_refresh()
            }
            AppWakeOwner::Subtitles => self.app_state.poll_subtitle_jobs(),
            AppWakeOwner::LocalFileOpen => self
                .app_state
                .poll_local_file_open_job(&mut self.playlist_runtime, self.renderer.as_mut()),
            AppWakeOwner::PlaylistRuntime => self.drain_queue_owners(),
            // Эти владельцы принадлежат обычному окну или ядру процесса.
            AppWakeOwner::StartupMedia
            | AppWakeOwner::InstanceForwarding
            | AppWakeOwner::BrowserSession => false,
        };
        if visible_mutation {
            self.app_state.note_visible_mutation_for_redraw();
            self.window.request_redraw();
        }
    }

    /// Оконное событие: ввод egui, ресайз, закрытие, кадр.
    pub(super) fn on_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) -> InstanceSignal {
        self.app_state
            .sync_committed_config_snapshot(self.settings_runtime.committed_snapshot());
        let egui_response = self
            .app_state
            .egui_winit_state
            .on_window_event(&self.window, &event);
        let redraw_after_event = should_request_redraw_after_window_event(&event);
        if redraw_after_event {
            self.app_state.note_window_input_for_redraw();
        }
        // Хоткеи оболочки — тем же классификатором и исполнителями, что у обычного окна.
        // Пробел и стрелки уходят в единую точку транспорта `AppState`, а в этом окне она
        // превращает их в намерения зрителя (их забирает цикл: `take_viewer_transport_intents`).
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = &event
            && handle_shell_key_event(
                &self.window,
                key_event,
                &mut self.app_state,
                &mut self.playlist_runtime,
                self.renderer.as_mut(),
            ) == ShellKeyOutcome::Handled
        {
            return InstanceSignal::Continue;
        }
        if egui_response.consumed {
            if redraw_after_event {
                self.window.request_redraw();
            }
            return InstanceSignal::Continue;
        }
        match event {
            WindowEvent::CloseRequested => return InstanceSignal::CloseRequested,
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                debug!(width, height, "Browser window resized");
                self.renderer_lifecycle
                    .resize_renderer(self.renderer.as_mut(), width, height);
            }
            WindowEvent::RedrawRequested => return self.render(event_loop),
            _ => {}
        }
        if redraw_after_event {
            self.window.request_redraw();
        }
        InstanceSignal::Continue
    }

    /// Перед сном цикла: решение планировщика кадров обычного окна.
    pub(super) fn before_idle_wait(
        &mut self,
        driver: &WindowPlaybackDriver,
    ) -> RedrawControlAction {
        let action = self.scheduler.before_idle_wait(
            self.app_state.wants_continuous_redraw(),
            self.has_pending_background_job(driver),
            Instant::now(),
        );
        self.note_scheduled_cause(action);
        action
    }

    /// Окно для применения решения планировщика.
    pub(super) fn window(&self) -> &Window {
        &self.window
    }

    /// Закрывает экземпляр: плеер, очередь, фоновые задачи настроек — в пределах срока.
    /// Config не сбрасывается: окно его не пишет (решение 122).
    pub(super) fn shut_down(mut self, deadline: ShutdownDeadline) {
        let player = self.app_state.shutdown_player_until(deadline);
        let queue = self.playlist_runtime.shutdown_until(deadline);
        let options = self
            .settings_runtime
            .shutdown_dynamic_options_until(deadline);
        info!(?player, ?options, queue = ?queue_outcome_label(&queue), "Browser window closed");
        // Ящики владельцев уничтожаются вместе с экземпляром: их висящие флаги
        // пробуждения не должны мешать следующему экземпляру.
        self.local_file_open_wake_port
            .acknowledge_abandoned_mailbox();
        self.subtitle_wake_port.acknowledge_abandoned_mailbox();
        self.player_timeline_wake_port
            .acknowledge_abandoned_mailbox();
    }

    /// Кадр: тот же `render_frame`, что у обычного окна.
    fn render(&mut self, event_loop: &ActiveEventLoop) -> InstanceSignal {
        let frame = render_frame(
            &self.telemetry,
            &self.window,
            &mut self.renderer,
            &mut self.app_state,
            &mut self.playlist_runtime,
            &mut self.settings_runtime,
            &mut self.renderer_lifecycle,
        );
        if frame.close_requested {
            return InstanceSignal::CloseRequested;
        }
        let has_pending = self.app_state.has_pending_prepared_media_strong()
            || self.app_state.has_pending_local_file_open()
            || self.settings_runtime.has_pending_options_refresh()
            || self.settings_runtime.has_pending_apply();
        let action = self.scheduler.after_render_with_deadline(
            frame.pacing,
            has_pending,
            frame.next_ui_wake_deadline,
            Instant::now(),
        );
        self.note_scheduled_cause(action);
        apply_redraw_action(event_loop, &self.window, action);
        InstanceSignal::Continue
    }

    /// Нужно ли циклу просыпаться ради фоновых задач экземпляра.
    fn has_pending_background_job(&self, driver: &WindowPlaybackDriver) -> bool {
        driver.has_load_in_progress()
            || self.app_state.has_pending_prepared_media_strong()
            || self.app_state.has_pending_local_file_open()
            || self.settings_runtime.has_pending_options_refresh()
            || self.settings_runtime.has_pending_apply()
            || self
                .playlist_runtime
                .has_pending_playlist_persistence_work()
    }

    /// Страховочный кадр воспроизведения помечается так же, как в обычном окне.
    fn note_scheduled_cause(&mut self, action: RedrawControlAction) {
        if action.redraw_cause == Some(RedrawCause::PlaybackFallback) {
            self.app_state.note_redraw_fallback_probe();
        }
    }

    /// Ящики очереди: импорт, обнаружение, состояние (без файлов — только память).
    fn drain_queue_owners(&mut self) -> bool {
        let mailbox_changed = self.playlist_runtime.drain_owner_mailbox();
        let persistence_changed = match self.playlist_runtime.drain_playlist_persistence() {
            Ok(changed) => changed,
            Err(error) => {
                warn!(?error, "Could not apply a browser window queue event");
                false
            }
        };
        let discovery_changed = self.playlist_runtime.drain_playlist_discovery();
        mailbox_changed || persistence_changed || discovery_changed
    }
}

/// Решение планировщика → winit (как `AppShell::apply_redraw_control_action`).
pub(super) fn apply_redraw_action(
    event_loop: &ActiveEventLoop,
    window: &Window,
    action: RedrawControlAction,
) {
    event_loop.set_control_flow(action.control_flow);
    if action.request_redraw() {
        window.request_redraw();
    }
}

/// Метка старта рендера обычного плеера — только для чтения.
fn renderer_startup_marker() -> ReadOnlyRendererStartupMarker {
    match fastiplayer_config::ConfigPaths::discover() {
        Ok(paths) => ReadOnlyRendererStartupMarker::read_from(paths.renderer_startup_marker_file()),
        Err(_) => ReadOnlyRendererStartupMarker::absent(),
    }
}

/// Ёмкость памяти дорожек из `[playlist]`; вне диапазона — максимум (как у обычного окна).
fn track_memory_capacity(configured_entries: u32) -> playlist_state::TrackMemoryCapacity {
    usize::try_from(configured_entries)
        .ok()
        .and_then(|entries| playlist_state::TrackMemoryCapacity::new(entries).ok())
        .unwrap_or(playlist_state::TrackMemoryCapacity::HARD_MAX)
}

/// Короткая метка итога закрытия очереди для журнала.
fn queue_outcome_label(
    outcome: &crate::playlist_runtime::PlaylistTerminalShutdownOutcome,
) -> &'static str {
    use crate::playlist_runtime::PlaylistTerminalShutdownOutcome as Outcome;
    match outcome {
        Outcome::Completed(_) => "completed",
        Outcome::AlreadyCompleted => "already_completed",
        Outcome::ExitRequired(_) => "exit_required",
        Outcome::Failed(_) => "failed",
    }
}
