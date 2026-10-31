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

mod dropped_collection_walk;
mod escape_dismissal;
mod event_loop;
mod external_drop;
mod hotkey_dispatch;
mod hotkeys;
mod instance_forwarding;
mod main_window_attributes;
mod relaunch_request;
mod renderer_startup_marker;
mod resolved_url_collection;
mod runtime_settings_flush;
mod scale_factor;
mod shutdown;
mod track_memory_flush;
mod window_identity;
mod window_limits;

// Детали обычного окна, которые переиспользует окно браузерного режима (мост 04a).
pub(crate) use hotkey_dispatch::{ShellKeyOutcome, handle_shell_key_event};
pub(crate) use main_window_attributes::main_window_attributes;
pub(crate) use renderer_startup_marker::ReadOnlyRendererStartupMarker;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use crate::app_instance::{AppInstanceLease, CommandLineStartup};
use crate::app_wake::{AppWakeOwner, AppWakeProxy};
use crate::render_backend::{StartedRenderer, create_startup_renderer};
use crate::render_settings::warn_legacy_tone_mapping_config;
use crate::system_capabilities::probe_system_capabilities;
use fastiplayer_config::{ConfigPaths, LoadedConfig};

use crate::config_startup_notice::config_startup_warning;
use crate::fatal_startup::{
    FatalErrorPhase, FatalStartupError, FatalStartupErrorSlot, FatalStartupReason,
};
use playlist_state::{PlaylistResumeStore, PlaylistStateStore};
use render_shell_api::RenderShell;
use tracing::info;
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::local_file_open::{LocalFileOpenJob, LocalFileOpenRestoreOutcome};
use crate::playlist_runtime::PlaylistRuntime;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};
use crate::redraw_pacing::{BackgroundPollScheduler, RedrawControlAction};
use crate::renderer_recreation::RendererLifecycleCoordinator;
use crate::settings_runtime::{HardwareDecodeAvailability, SettingsRuntime};
use crate::startup_media::StartupMediaController;
use crate::state::{
    AppState, AppStateStartupContext, RendererVideoImportTarget, TrackMemoryHandle,
};
use crate::telemetry::Telemetry;
use renderer_startup_marker::RendererStartupWatch;
use shutdown::{
    AppShellProcessLifecycle, AppShellShutdownReport, OwnerTerminalDisposition,
    PROCESS_TERMINAL_SHUTDOWN_BUDGET, TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE,
    TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET, TerminalEntryDisposition, lifecycle_after_shutdown,
    process_owner_disposition, report_timeline_seek_lifecycle_settlement,
    terminal_entry_disposition,
};

/// Winit shell приложения.
///
/// Владеет:
/// - окном (`Arc<Window>`)
/// - рендерером (`render-wgpu-shell` backend)
/// - состоянием приложения (egui + player state)
/// - телеметрией
/// - controller-ом стартового media lifecycle
pub(crate) struct AppShell {
    /// Monotonic process origin снимается до config/instance bootstrap-а.
    process_started_at: Instant,

    /// Окно приложения. None до Resumed.
    window: Option<Arc<Window>>,

    /// Рендерер с GPU ресурсами. None до Resumed.
    renderer: Option<Box<dyn RenderShell>>,

    /// Состояние приложения (egui, player state). None до Resumed.
    app_state: Option<AppState>,

    /// Общая телеметрия.
    telemetry: Arc<Telemetry>,

    /// Controller стартового media и фоновой подготовки CLI YtDlp URL.
    startup_media: StartupMediaController,

    /// Process-lifetime playlist owner переживает renderer-bound AppState recreation.
    playlist_runtime: PlaylistRuntime,

    /// Port копируется в каждый renderer-bound AppState при resume.
    local_file_open_wake_port: crate::app_wake::AppWakePort,

    /// Payload-free bridge для dynamic timeline worker revision.
    player_timeline_wake_port: crate::app_wake::AppWakePort,

    /// Port фоновой загрузки субтитров; копируется в каждый renderer-bound AppState.
    subtitle_wake_port: crate::app_wake::AppWakePort,

    /// Приём файлов от следующих запусков (сессия 13); process-lifetime, как lease.
    instance_forwarding: crate::instance_forwarding::InstanceForwardingInbox,
    /// `--fullscreen` первого запуска; применяется один раз, когда окно готово (06b).
    startup_fullscreen: desktop_integration::FullscreenIntent,

    /// Exact local-file job переживает renderer suspend без detach или process flush.
    suspended_local_file_open_job: Option<LocalFileOpenJob>,

    /// Authoritative runtime owner пользовательских настроек.
    settings_runtime: SettingsRuntime,

    /// Предупреждение об итоге загрузки config-а; забирается первым созданным `AppState`.
    pending_config_warning: Option<String>,

    /// Scheduler idle wakeup-ов для shell background jobs.
    background_poll_scheduler: BackgroundPollScheduler,

    /// Сериализатор renderer recreation и surface resize lifecycle-а.
    renderer_lifecycle: RendererLifecycleCoordinator,

    /// Terminal state запрещает повторные UI actions после начала process close.
    process_lifecycle: AppShellProcessLifecycle,

    /// Первая фатальная ошибка запуска; `main` показывает её после shutdown.
    fatal_startup_error: FatalStartupErrorSlot,

    /// Фаза, в которой окажется следующая фатальная ошибка. `Startup`, пока runtime
    /// ни разу не поднялся целиком; после первого успешного `restore_runtime` —
    /// `RuntimeRecreation`: любой провал пересоздания после сна/сворачивания
    /// показывается как потеря работавшего плеера, а не как «ошибка запуска».
    fatal_error_phase: FatalErrorPhase,

    /// Память выбора дорожек «для каждого видео»: живёт весь процесс, `AppState` её только
    /// использует (пересоздание после suspend выбор не теряет).
    track_memory: TrackMemoryHandle,

    /// Метка «рендер не дошёл до первого кадра»: пишется перед созданием, снимается после
    /// первого показанного кадра или штатной остановки рендера.
    renderer_startup: RendererStartupWatch,

    /// Единственный platform path owner для будущего state worker integration.
    _config_paths: ConfigPaths,

    /// Process lease освобождается последним после остальных полей shell-а.
    _instance_lease: AppInstanceLease,
}

impl AppShell {
    /// Создаёт пустой shell.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    pub(crate) fn new(
        process_started_at: Instant,
        command_line: CommandLineStartup,
        loaded_config: LoadedConfig,
        wake_proxy: AppWakeProxy,
        config_paths: ConfigPaths,
        instance_lease: AppInstanceLease,
    ) -> anyhow::Result<Self> {
        let startup_wake_port = wake_proxy.port(AppWakeOwner::StartupMedia);
        let local_file_wake_port = wake_proxy.port(AppWakeOwner::LocalFileOpen);
        let settings_wake_port = wake_proxy.port(AppWakeOwner::SettingsDynamicOptions);
        let playlist_wake_port = wake_proxy.port(AppWakeOwner::PlaylistRuntime);
        let player_timeline_wake_port = wake_proxy.port(AppWakeOwner::PlayerTimeline);
        let subtitle_wake_port = wake_proxy.port(AppWakeOwner::Subtitles);
        let instance_forwarding_wake_port = wake_proxy.port(AppWakeOwner::InstanceForwarding);
        // Сначала строятся все fallible process owners. Inspection запускается
        // последней: после неё constructor уже не может вернуть ошибку и detach-нуть thread.
        let pending_config_warning = config_startup_warning(&loaded_config);
        let mut settings_runtime =
            SettingsRuntime::from_loaded_config_with_wake_port(loaded_config, settings_wake_port)?;
        // Общая доска (04c): изменения настроек из окон браузерного режима — на лету.
        settings_runtime.start_watching_config_file(wake_proxy.port(AppWakeOwner::ConfigFile));
        let mut playlist_runtime = PlaylistRuntime::new_with_resume_policy(
            playlist_wake_port,
            settings_runtime.committed_config().playlist,
            settings_runtime
                .committed_config()
                .player
                .resume_last_position,
        );
        playlist_runtime.install_playlist_resume_store(Arc::new(PlaylistResumeStore::new(
            config_paths.playlist_resume_file(),
        )));
        // Constructor доступен только после process bootstrap с acquired lease.
        // MPRIS сразу видит реально слышимую громкость: при сохранённом mute это 0.
        playlist_runtime.start_desktop_transport(
            settings_runtime
                .committed_snapshot()
                .user_audio_level()
                .effective_player_volume(),
        );
        playlist_runtime
            .begin_production_playlist_state_inspection(Arc::new(PlaylistStateStore::new(
                config_paths.playlist_state_file(),
            )))
            .map_err(|error| {
                anyhow::anyhow!("playlist state inspection startup failed: {error:?}")
            })?;
        // Память выбора дорожек: файл рядом с config; повреждённый уходит в карантин.
        let track_memory = track_memory_flush::start_for_shell(&config_paths, &settings_runtime);
        // Под уже взятым lease: имя приложения на шине принадлежит единственному
        // экземпляру. Запускается как можно раньше, чтобы второй запуск ждал недолго.
        let instance_forwarding = crate::instance_forwarding::InstanceForwardingInbox::start(
            instance_forwarding_wake_port,
        );
        Ok(Self {
            process_started_at,
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            startup_media: StartupMediaController::with_wake_port(
                command_line.initial_media,
                command_line.startup_error,
                startup_wake_port,
            ),
            playlist_runtime,
            local_file_open_wake_port: local_file_wake_port,
            player_timeline_wake_port,
            subtitle_wake_port,
            instance_forwarding,
            startup_fullscreen: command_line.fullscreen,
            suspended_local_file_open_job: None,
            settings_runtime,
            pending_config_warning,
            background_poll_scheduler: BackgroundPollScheduler::new(),
            renderer_lifecycle: RendererLifecycleCoordinator::default(),
            process_lifecycle: AppShellProcessLifecycle::Running,
            fatal_startup_error: FatalStartupErrorSlot::default(),
            fatal_error_phase: FatalErrorPhase::Startup,
            track_memory,
            renderer_startup: RendererStartupWatch::new(
                config_paths.renderer_startup_marker_file(),
            ),
            _config_paths: config_paths,
            _instance_lease: instance_lease,
        })
    }

    /// Создаёт или пересоздаёт runtime-ресурсы, завязанные на активное окно.
    ///
    /// Winit 0.30 вызывает `resumed` не только при первом старте, но и после возврата
    /// приложения из suspended-состояния. Окно при этом может уже существовать, а surface
    /// и GPU-ресурсы могли быть сброшены. Поэтому восстановление renderer/app_state
    /// отделено от создания окна.
    fn restore_runtime(&mut self, event_loop: &ActiveEventLoop, window: Arc<Window>) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        if self.renderer.is_some() && self.app_state.is_some() {
            return;
        }
        let runtime_restore_started_at = Instant::now();

        // Стартовые настройки цвета нужны фабрике рендера (прежние `set_*` до первого кадра).
        let initial_render_settings = match self.settings_runtime.initial_render_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.exit_with_fatal_startup_error(
                    event_loop,
                    FatalStartupError::internal_failure("Incorrect render color settings", &error),
                );
                return;
            }
        };
        warn_legacy_tone_mapping_config(self.settings_runtime.committed_config());
        // Метка относится к конкретному созданию: кадры прошлого рендера её не снимают.
        self.renderer_startup
            .begin_startup(self.telemetry.render_frame_totals());
        let StartedRenderer {
            mut renderer,
            report: renderer_report,
        } = match create_startup_renderer(
            self.settings_runtime.committed_config(),
            window.clone(),
            initial_render_settings,
            &mut self.renderer_startup,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.exit_with_fatal_startup_error(
                    event_loop,
                    FatalStartupError::renderer_unavailable(&error),
                );
                return;
            }
        };
        // Семейство рендера для подсказки в настройках; одного вызова при старте достаточно.
        self.settings_runtime
            .set_active_renderer_family(renderer_report.family());
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup window/GPU renderer ready"
        );

        let mut app_state = match AppState::new(
            &window,
            AppStateStartupContext::new(
                self.process_started_at,
                self.startup_media.startup_error_message(),
                self.pending_config_warning.take(),
                self.subtitle_wake_port.clone(),
                self.track_memory.clone(),
            )
            .with_renderer_report(renderer_report),
            self.telemetry.clone(),
            self.settings_runtime.committed_snapshot(),
            self.settings_runtime.audio_output_device_controller(),
            self.local_file_open_wake_port.clone(),
            self.player_timeline_wake_port.clone(),
        ) {
            Ok(app_state) => app_state,
            Err(error) => {
                self.exit_with_fatal_startup_error(
                    event_loop,
                    FatalStartupError::internal_failure(
                        "We failed to launch the app state.",
                        &error,
                    ),
                );
                return;
            }
        };
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup player worker ready"
        );
        let system_capabilities = probe_system_capabilities(renderer.capabilities().clone());
        info!("{}", system_capabilities.summary_text());
        // Окно настроек предупреждает о режиме «Hardware» по этому же отчёту (UX2 B6).
        self.settings_runtime.set_hardware_decode_availability(
            HardwareDecodeAvailability::from_system_capabilities(&system_capabilities),
        );
        app_state.set_system_capabilities(system_capabilities.clone());
        app_state.init_video_pipeline(RendererVideoImportTarget::for_renderer(renderer.as_mut()));
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup video pipeline ready"
        );

        let Some(playlist_binding) = self.playlist_runtime.bind_resumed_app_state() else {
            // Сначала фиксация (она же пишет деталь в лог): очистка ниже при
            // зависании завершает процесс аварийно, и деталь иначе потерялась бы.
            self.exit_with_fatal_startup_error(
                event_loop,
                FatalStartupError::new(
                    FatalStartupReason::InternalFailure,
                    "Playlist runtime is closed and does not accept new AppState binding",
                ),
            );
            Self::shutdown_uninstalled_app_state_or_exit(&mut app_state);
            return;
        };
        let playlist_attachment = match self.playlist_runtime.app_state_attachment(playlist_binding)
        {
            Ok(attachment) => attachment,
            Err(error) => {
                self.exit_with_fatal_startup_error(
                    event_loop,
                    FatalStartupError::internal_failure(
                        "Playlist runtime does not provide exact AppState attachment",
                        &format!("{error:?}"),
                    ),
                );
                Self::shutdown_uninstalled_app_state_or_exit(&mut app_state);
                return;
            }
        };
        app_state.attach_playlist_runtime(playlist_attachment);
        self.playlist_runtime
            .attach_player_sender(app_state.player_command_sender());
        // UX-11: новый player binding получает уровень пользователя целиком
        // (громкость «до mute» + mute), а не только слышимое число, иначе после
        // включения звука вернулась бы громкость из fallback-а.
        let restored_audio_level = self
            .settings_runtime
            .begin_player_audio_level_restore(std::time::Instant::now());
        if let Err(error) = crate::user_audio_level::send_user_audio_level_to_player(
            &app_state.player_command_sender(),
            restored_audio_level,
        ) {
            tracing::warn!(error = %error, "User Loudness Not Accepted by New player binding");
        }
        let _resume_started = app_state.start_suspended_media_resume(&mut self.playlist_runtime);

        if let Some(transferred_job) = self.suspended_local_file_open_job.take() {
            match app_state.restore_local_file_open_job_after_resume(transferred_job) {
                LocalFileOpenRestoreOutcome::Restored => {}
                LocalFileOpenRestoreOutcome::ExistingJob(rejected_job) => {
                    self.suspended_local_file_open_job = Some(*rejected_job);
                    self.shutdown_rejected_local_job_or_exit();
                }
            }
        }

        self.startup_media.start_pending_initial_media(
            &mut app_state,
            &mut self.playlist_runtime,
            &mut *renderer,
            self.settings_runtime.committed_config(),
            &system_capabilities,
        );
        self.startup_media.poll_startup_jobs(
            &mut app_state,
            &mut self.playlist_runtime,
            &mut *renderer,
        );
        app_state.poll_local_file_open_job(&mut self.playlist_runtime, &mut *renderer);

        // Shell явно разделяет обновление snapshot и публикацию в desktop integration.
        let player_snapshot = app_state.refresh_player_snapshot();
        self.playlist_runtime
            .publish_desktop_snapshot(&player_snapshot);

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        // Runtime поднялся целиком: дальнейшие провалы пересоздания — уже не запуск.
        self.fatal_error_phase = FatalErrorPhase::RuntimeRecreation;
        // `--fullscreen` запуска и запросы второго запуска, ждавшие окна, исполняются сейчас.
        self.execute_pending_window_requests();
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn suspend_runtime(&mut self) {
        self.flush_runtime_settings_for_lifecycle_boundary();
        self.flush_track_memory_for_suspend();
        let timeline_seek_deadline = Instant::now() + TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET;
        let mut transferred_local_file_open_job = None;
        if let Some(app_state) = &mut self.app_state {
            let binding = app_state.playlist_runtime_binding();
            let resume_checkpoint_already_exists =
                self.playlist_runtime.has_suspended_media_checkpoint();
            let lifecycle_result = app_state
                .resolve_same_item_switch_for_suspend(
                    &mut self.playlist_runtime,
                    self.renderer.as_mut(),
                )
                .and_then(|()| {
                    app_state.cancel_suspended_media_resume_for_suspend(&mut self.playlist_runtime)
                })
                .and_then(|()| self.playlist_runtime.resolve_pending_media_for_suspend())
                .and_then(|()| {
                    if resume_checkpoint_already_exists {
                        // Mid-resume suspend уже re-arm-нул прежний process checkpoint;
                        // released candidate больше не обязан совпадать со старым instance.
                        return Ok(());
                    }
                    let pre_settlement_snapshot = app_state.refresh_player_snapshot();
                    let settlement = app_state.settle_pending_timeline_seek_for_lifecycle(
                        &mut self.playlist_runtime,
                        timeline_seek_deadline,
                        pre_settlement_snapshot.current_position,
                    );
                    report_timeline_seek_lifecycle_settlement(settlement);
                    let snapshot = app_state.refresh_player_snapshot();
                    let binding = binding.ok_or(
                        crate::playlist_runtime::ResumeCheckpointError::StalePlayerBinding,
                    )?;
                    self.playlist_runtime
                        .capture_suspended_media_checkpoint_after_seek_settlement(
                            binding,
                            &snapshot,
                            settlement.checkpoint_position_policy(),
                        )
                        .map(|_| ())
                });
            if let Err(error) = lifecycle_result {
                tracing::error!(
                    ?error,
                    "Suspend active-media checkpoint ends with lifecycle invariant failure"
                );
            }
            app_state.clear_cached_present_frame_for_runtime_drop();
            transferred_local_file_open_job = app_state.take_local_file_open_job_for_suspend();
        }

        if let Some(mut transferred_job) = transferred_local_file_open_job {
            if self.suspended_local_file_open_job.is_none() {
                self.suspended_local_file_open_job = Some(transferred_job);
            } else {
                tracing::error!(
                    "Suspend second-hand local-file owner; rejected job finish bounded"
                );
                Self::shutdown_local_job_owner_or_exit(&mut transferred_job);
            }
        }

        self.playlist_runtime.suspend_app_state_binding();
        self.playlist_runtime.publish_detached_desktop_snapshot();
        self.app_state = None;
        self.renderer = None;
        self.renderer_startup
            .renderer_closed(self.telemetry.render_frame_totals());
    }

    /// Закрывает приложение через единый cleanup path shell-а.
    fn close_runtime_and_exit(&mut self, event_loop: &ActiveEventLoop, reason: &'static str) {
        info!("{reason}");
        self.finish_process_shutdown();
        event_loop.exit();
    }

    /// Фиксирует фатальную ошибку запуска и останавливает event loop.
    ///
    /// Ошибка не показывается здесь: окно ошибки блокирует поток, а owners
    /// (player, playlist persistence, lease) должны сначала штатно завершиться.
    /// `main` заберёт её через `take_fatal_startup_error` после `run_app`.
    fn exit_with_fatal_startup_error(
        &mut self,
        event_loop: &ActiveEventLoop,
        error: FatalStartupError,
    ) {
        // Фатальная ошибка — штатная остановка, а не падение драйвера: метку снимаем.
        self.renderer_startup
            .renderer_closed(self.telemetry.render_frame_totals());
        self.fatal_startup_error
            .record(error.in_phase(self.fatal_error_phase));
        event_loop.exit();
    }

    /// Забирает фатальную ошибку, из-за которой остановился event loop.
    pub(crate) fn take_fatal_startup_error(&mut self) -> Option<FatalStartupError> {
        self.fatal_startup_error.take()
    }

    /// Завершает process owners после возврата event loop либо из `exiting`.
    pub(crate) fn finish_process_shutdown(&mut self) {
        match terminal_entry_disposition(self.process_lifecycle) {
            TerminalEntryDisposition::AlreadyCompleted => return,
            TerminalEntryDisposition::ExitRequired => {
                tracing::error!(
                    "Repeated terminal entrance caught incomplete shutdown; lease stays on"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            TerminalEntryDisposition::Begin => {
                self.process_lifecycle = AppShellProcessLifecycle::ShuttingDown;
            }
        }
        // Приём от следующих запусков закрывается первым: им честно ответят
        // «плеер закрывается», а не примут файл, который уже некому открыть.
        self.instance_forwarding.shutdown();

        self.flush_runtime_settings_for_lifecycle_boundary();
        self.shutdown_track_memory();
        let deadline = ShutdownDeadline::after(PROCESS_TERMINAL_SHUTDOWN_BUDGET);
        if let Some(app_state) = self.app_state.as_mut()
            && let Some(binding) = app_state.playlist_runtime_binding()
        {
            let pre_settlement_snapshot = app_state.refresh_player_snapshot();
            let timeline_seek_deadline = (Instant::now()
                + TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET)
                .min(deadline.expires_at());
            let settlement = app_state.settle_pending_timeline_seek_for_lifecycle(
                &mut self.playlist_runtime,
                timeline_seek_deadline,
                pre_settlement_snapshot.current_position,
            );
            report_timeline_seek_lifecycle_settlement(settlement);
            let snapshot = app_state.refresh_player_snapshot();
            self.playlist_runtime
                .force_resume_checkpoint_after_seek_settlement(
                    binding,
                    &snapshot,
                    settlement.checkpoint_position_policy(),
                );
        }

        // Local job сначала извлекается из renderer owner-а, чтобы любой timeout
        // сохранил exact handle внутри process-lifetime AppShell.
        if self.suspended_local_file_open_job.is_none()
            && let Some(app_state) = self.app_state.as_mut()
        {
            self.suspended_local_file_open_job = app_state.take_local_file_open_job_for_suspend();
        }

        // Внешний command admission закрывается до player owner-а; lease остаётся последним.
        let desktop_integration = self
            .playlist_runtime
            .shutdown_desktop_transport_until(deadline);
        let player = self
            .app_state
            .as_mut()
            .map(|app_state| app_state.shutdown_player_until(deadline));
        let local_file_open = self
            .suspended_local_file_open_job
            .as_mut()
            .map_or(ProcessOwnerShutdownOutcome::AlreadyCompleted, |job| {
                job.shutdown_until(deadline)
            });
        if !matches!(
            local_file_open,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) {
            self.suspended_local_file_open_job = None;
        }
        let startup_media = self.startup_media.shutdown_until(deadline);
        let settings = self
            .settings_runtime
            .shutdown_dynamic_options_until(deadline);
        let playlist = self.playlist_runtime.shutdown_until(deadline);
        let report = AppShellShutdownReport {
            desktop_integration,
            player,
            local_file_open,
            startup_media,
            settings,
            playlist,
        };

        match report.terminal_disposition() {
            OwnerTerminalDisposition::ExitRequired => {
                tracing::error!(
                    ?report,
                    "Process owners are not completed before the general deadline; immediately complete the process with the retained lease"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(
                    ?report,
                    "Process owners are terminal, but the shutdown ended with a typed error"
                );
            }
            OwnerTerminalDisposition::Completed => {
                info!(?report, "Process owners properly completed");
            }
        }

        // До этой точки timeout не может дойти: `process::exit` не запускает Drop.
        self.playlist_runtime.suspend_app_state_binding();
        self.app_state = None;
        self.renderer = None;
        self.renderer_startup
            .renderer_closed(self.telemetry.render_frame_totals());
        self.process_lifecycle = lifecycle_after_shutdown(report.terminal_disposition());
    }

    fn shutdown_rejected_local_job_or_exit(&mut self) {
        if let Some(job) = self.suspended_local_file_open_job.as_mut() {
            Self::shutdown_local_job_owner_or_exit(job);
        }
        self.suspended_local_file_open_job = None;
    }

    /// Общая terminal policy для local job, который нельзя сохранить в shell slot-е.
    fn shutdown_local_job_owner_or_exit(job: &mut LocalFileOpenJob) {
        let outcome = job.shutdown_until(ShutdownDeadline::after(PROCESS_TERMINAL_SHUTDOWN_BUDGET));
        match process_owner_disposition(outcome) {
            OwnerTerminalDisposition::ExitRequired => {
                tracing::error!(?outcome, "Local-file owner is not bounded.");
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(?outcome, "Local-file owner finished panic");
            }
            OwnerTerminalDisposition::Completed => {}
        }
    }

    /// Применяет уже принятое scheduler-ом решение к winit и окну.
    fn apply_redraw_control_action(
        event_loop: &ActiveEventLoop,
        window: Option<&Window>,
        action: RedrawControlAction,
    ) {
        event_loop.set_control_flow(action.control_flow);

        if action.request_redraw()
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
                .is_some_and(AppState::has_pending_prepared_media_strong)
            || self
                .app_state
                .as_ref()
                .is_some_and(AppState::has_pending_local_file_open)
            || self.settings_runtime.has_pending_options_refresh()
            || self.settings_runtime.has_pending_apply()
            || self
                .app_state
                .as_ref()
                .is_some_and(AppState::has_pending_suspended_media_resume)
            || self
                .playlist_runtime
                .has_pending_playlist_persistence_work()
    }

    /// Единая PlaylistRuntime wake точка применяет startup/save outcomes на UI thread.
    fn drain_playlist_persistence(&mut self) -> bool {
        self.playlist_runtime.drain_owner_mailbox();
        let desktop_commands = self.playlist_runtime.drain_desktop_commands();
        let desktop_changed = match (self.app_state.as_mut(), self.renderer.as_deref_mut()) {
            (Some(app_state), Some(renderer)) => {
                let snapshot = app_state.refresh_player_snapshot();
                let changed = crate::transport_runtime::apply_desktop_commands(
                    app_state,
                    &mut self.playlist_runtime,
                    &mut self.startup_media,
                    renderer,
                    &snapshot,
                    desktop_commands,
                );
                self.playlist_runtime.publish_desktop_snapshot(&snapshot);
                changed
            }
            _ => false,
        };
        let persistence_changed = match self.playlist_runtime.drain_playlist_persistence() {
            Ok(visible_change) => visible_change,
            Err(error) => {
                tracing::error!(error = %error, "Could not apply playlist persistence event");
                false
            }
        };
        let discovery_changed = self.playlist_runtime.drain_playlist_discovery();
        let resume_changed = match (self.app_state.as_mut(), self.renderer.as_deref_mut()) {
            (Some(app_state), Some(renderer)) => {
                app_state.drive_suspended_media_resume(&mut self.playlist_runtime, renderer)
            }
            _ => false,
        };
        let startup_changed = match (self.app_state.as_mut(), self.renderer.as_deref_mut()) {
            (Some(app_state), Some(renderer)) => self.startup_media.poll_startup_jobs(
                app_state,
                &mut self.playlist_runtime,
                renderer,
            ),
            _ => false,
        };
        let walk_changed = self.drain_dropped_collection_walk();
        let url_collection_changed = self.drain_resolved_url_collection();
        let _resume_status = self.playlist_runtime.suspended_media_status();
        desktop_changed
            || walk_changed
            || url_collection_changed
            || persistence_changed
            || discovery_changed
            || resume_changed
            || startup_changed
    }
}
