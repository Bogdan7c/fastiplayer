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

mod event_loop;
mod hotkeys;
mod shutdown;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use crate::app_instance::AppInstanceLease;
use crate::app_wake::{AppWakeOwner, AppWakeProxy};
use crate::render_settings::{
    surface_present_settings_from_config, warn_legacy_tone_mapping_config,
};
use crate::system_capabilities::probe_system_capabilities;
use fastiplayer_config::{ConfigPaths, LoadedConfig};
use playlist_state::{PlaylistResumeStore, PlaylistStateStore};
use render_wgpu_shell::Renderer;
use tracing::{debug, info};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::frame_prepare::flush_sidebar_resize_before_lifecycle_boundary;
use crate::local_file_open::{LocalFileOpenJob, LocalFileOpenRestoreOutcome};
use crate::playlist_runtime::PlaylistRuntime;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};
use crate::redraw_pacing::{BackgroundPollScheduler, RedrawControlAction};
use crate::renderer_recreation::RendererLifecycleCoordinator;
use crate::settings_runtime::SettingsRuntime;
use crate::startup_media::{InitialMedia, StartupMediaController};
use crate::state::{AppState, AppStateStartupContext, LifecycleTimelineSeekSettlement};
use crate::telemetry::Telemetry;
use shutdown::{
    AppShellProcessLifecycle, AppShellShutdownReport, OwnerTerminalDisposition,
    PROCESS_TERMINAL_SHUTDOWN_BUDGET, TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE,
    TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET, TerminalEntryDisposition, player_disposition,
    process_owner_disposition, terminal_entry_disposition,
};

/// Публикует typed lifecycle settlement без выдачи timeout-а за успешный seek commit.
fn report_timeline_seek_lifecycle_settlement(settlement: LifecycleTimelineSeekSettlement) {
    match settlement {
        LifecycleTimelineSeekSettlement::NoPendingSeek => {}
        LifecycleTimelineSeekSettlement::Settled {
            checkpoint_position,
        } => debug!(
            ?checkpoint_position,
            "Lifecycle дождался terminal outcomes всех pending timeline seek-ов"
        ),
        LifecycleTimelineSeekSettlement::DeadlineElapsed {
            checkpoint_position,
            abandoned_receipt_count,
        } => tracing::warn!(
            ?checkpoint_position,
            abandoned_receipt_count,
            "Lifecycle deadline отменил pending timeline seek; сохраняем pre-seek позицию"
        ),
        LifecycleTimelineSeekSettlement::MissingOwnerOutcome {
            checkpoint_position,
            abandoned_receipt_count,
        } => tracing::error!(
            ?checkpoint_position,
            abandoned_receipt_count,
            "Player owner потерял pending timeline seek outcome; сохраняем pre-seek позицию"
        ),
    }
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
    /// Monotonic process origin снимается до config/instance bootstrap-а.
    process_started_at: Instant,

    /// Окно приложения. None до Resumed.
    window: Option<Arc<Window>>,

    /// Рендерер с GPU ресурсами. None до Resumed.
    renderer: Option<Renderer>,

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

    /// Exact local-file job переживает renderer suspend без detach или process flush.
    suspended_local_file_open_job: Option<LocalFileOpenJob>,

    /// Authoritative runtime owner пользовательских настроек.
    settings_runtime: SettingsRuntime,

    /// Scheduler idle wakeup-ов для shell background jobs.
    background_poll_scheduler: BackgroundPollScheduler,

    /// Сериализатор renderer recreation и surface resize lifecycle-а.
    renderer_lifecycle: RendererLifecycleCoordinator,

    /// Terminal state запрещает повторные UI actions после начала process close.
    process_lifecycle: AppShellProcessLifecycle,

    /// Единственный platform path owner для будущего state worker integration.
    _config_paths: ConfigPaths,

    /// Process lease освобождается последним после остальных полей shell-а.
    _instance_lease: AppInstanceLease,
}

impl AppShell {
    /// Сохраняет pending sidebar resize до уничтожения renderer-bound geometry owner-а.
    fn flush_sidebar_resize_for_lifecycle_boundary(&mut self) {
        if self
            .settings_runtime
            .next_sidebar_resize_deadline()
            .is_none()
        {
            return;
        }
        let (Some(window), Some(renderer), Some(app_state)) = (
            self.window.as_ref(),
            self.renderer.as_mut(),
            self.app_state.as_mut(),
        ) else {
            tracing::error!(
                "Pending sidebar resize дошёл до lifecycle boundary без активного AppState"
            );
            return;
        };

        if let Err(error) = flush_sidebar_resize_before_lifecycle_boundary(
            window,
            renderer,
            app_state,
            &mut self.playlist_runtime,
            &mut self.settings_runtime,
            &mut self.renderer_lifecycle,
        ) {
            self.settings_runtime.report_runtime_error(
                "Не удалось сохранить ширину sidebar перед suspend/exit",
                error,
            );
        }
    }

    /// Создаёт пустой shell.
    ///
    /// Ресурсы инициализируются в Resumed, когда окно готово.
    pub(crate) fn new(
        process_started_at: Instant,
        initial_media: Option<InitialMedia>,
        startup_error: Option<String>,
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
        // Сначала строятся все fallible process owners. Inspection запускается
        // последней: после неё constructor уже не может вернуть ошибку и detach-нуть thread.
        let settings_runtime =
            SettingsRuntime::from_loaded_config_with_wake_port(loaded_config, settings_wake_port)?;
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
        playlist_runtime.start_desktop_transport(
            settings_runtime
                .committed_snapshot()
                .default_volume_for_new_media(),
        );
        playlist_runtime
            .begin_production_playlist_state_inspection(Arc::new(PlaylistStateStore::new(
                config_paths.playlist_state_file(),
            )))
            .map_err(|error| {
                anyhow::anyhow!("playlist state inspection startup failed: {error:?}")
            })?;
        Ok(Self {
            process_started_at,
            window: None,
            renderer: None,
            app_state: None,
            telemetry: Arc::new(Telemetry::new()),
            startup_media: StartupMediaController::with_wake_port(
                initial_media,
                startup_error,
                startup_wake_port,
            ),
            playlist_runtime,
            local_file_open_wake_port: local_file_wake_port,
            player_timeline_wake_port,
            suspended_local_file_open_job: None,
            settings_runtime,
            background_poll_scheduler: BackgroundPollScheduler::new(),
            renderer_lifecycle: RendererLifecycleCoordinator::default(),
            process_lifecycle: AppShellProcessLifecycle::Running,
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
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup window/GPU renderer ready"
        );
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
            AppStateStartupContext::new(
                self.process_started_at,
                self.startup_media.startup_error_message(),
            ),
            self.telemetry.clone(),
            self.settings_runtime.committed_snapshot(),
            self.settings_runtime.audio_output_device_controller(),
            self.local_file_open_wake_port.clone(),
            self.player_timeline_wake_port.clone(),
        ) {
            Ok(app_state) => app_state,
            Err(error) => {
                tracing::error!(error = %error, "Не удалось запустить app state");
                event_loop.exit();
                return;
            }
        };
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup player worker ready"
        );
        let system_capabilities = probe_system_capabilities(renderer.render_capabilities());
        info!("{}", system_capabilities.summary_text());
        app_state.set_system_capabilities(system_capabilities.clone());
        app_state.init_video_pipeline(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );
        info!(
            process_elapsed_ms = self.process_started_at.elapsed().as_secs_f64() * 1_000.0,
            runtime_restore_elapsed_ms =
                runtime_restore_started_at.elapsed().as_secs_f64() * 1_000.0,
            "Startup video pipeline ready"
        );

        let Some(playlist_binding) = self.playlist_runtime.bind_resumed_app_state() else {
            tracing::error!("Playlist runtime уже закрыт и не принимает новый AppState binding");
            Self::shutdown_uninstalled_app_state_or_exit(&mut app_state);
            event_loop.exit();
            return;
        };
        let playlist_attachment = match self.playlist_runtime.app_state_attachment(playlist_binding)
        {
            Ok(attachment) => attachment,
            Err(error) => {
                tracing::error!(
                    ?error,
                    "Playlist runtime не создал exact AppState attachment"
                );
                Self::shutdown_uninstalled_app_state_or_exit(&mut app_state);
                event_loop.exit();
                return;
            }
        };
        app_state.attach_playlist_runtime(playlist_attachment);
        self.playlist_runtime
            .attach_player_sender(app_state.player_command_sender());
        if let Err(error) =
            app_state
                .player_command_sender()
                .try_send(player_core::PlayerCommand::SetVolume(
                    self.playlist_runtime.desktop_effective_volume().as_player(),
                ))
        {
            tracing::warn!(error = %error, "Process-lifetime effective volume не принят новым player binding");
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
            &renderer,
            self.settings_runtime.committed_config(),
            &system_capabilities,
        );
        self.startup_media
            .poll_startup_jobs(&mut app_state, &mut self.playlist_runtime, &renderer);
        app_state.poll_local_file_open_job(&mut self.playlist_runtime, &renderer);

        // Shell явно разделяет обновление snapshot и публикацию в desktop integration.
        let player_snapshot = app_state.refresh_player_snapshot();
        self.playlist_runtime
            .publish_desktop_snapshot(&player_snapshot);

        self.renderer = Some(renderer);
        self.app_state = Some(app_state);
        window.request_redraw();
    }

    /// Освобождает runtime-ресурсы в порядке, безопасном для GPU/audio cleanup.
    fn suspend_runtime(&mut self) {
        self.flush_sidebar_resize_for_lifecycle_boundary();
        let timeline_seek_deadline = Instant::now() + TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET;
        let mut transferred_local_file_open_job = None;
        if let Some(app_state) = &mut self.app_state {
            let binding = app_state.playlist_runtime_binding();
            let resume_checkpoint_already_exists =
                self.playlist_runtime.has_suspended_media_checkpoint();
            let lifecycle_result = app_state
                .resolve_same_item_switch_for_suspend(&mut self.playlist_runtime)
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
                    "Suspend active-media checkpoint завершился lifecycle invariant failure"
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
                    "Suspend обнаружил второй local-file owner; rejected job завершается bounded"
                );
                Self::shutdown_local_job_owner_or_exit(&mut transferred_job);
            }
        }

        self.playlist_runtime.suspend_app_state_binding();
        self.playlist_runtime.publish_detached_desktop_snapshot();
        self.app_state = None;
        self.renderer = None;
    }

    /// Закрывает приложение через единый cleanup path shell-а.
    fn close_runtime_and_exit(&mut self, event_loop: &ActiveEventLoop, reason: &'static str) {
        info!("{reason}");
        self.finish_process_shutdown();
        event_loop.exit();
    }

    /// Завершает process owners после возврата event loop либо из `exiting`.
    pub(crate) fn finish_process_shutdown(&mut self) {
        match terminal_entry_disposition(self.process_lifecycle) {
            TerminalEntryDisposition::AlreadyCompleted => return,
            TerminalEntryDisposition::ExitRequired => {
                tracing::error!(
                    "Повторный terminal вход застал незавершённый shutdown; lease остаётся удержанным"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            TerminalEntryDisposition::Begin => {
                self.process_lifecycle = AppShellProcessLifecycle::ShuttingDown;
            }
        }

        self.flush_sidebar_resize_for_lifecycle_boundary();
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
                    "Process owners не завершились до общего deadline; немедленно завершаем process с удержанным lease"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(
                    ?report,
                    "Process owners терминальны, но shutdown завершился типизированной ошибкой"
                );
            }
            OwnerTerminalDisposition::Completed => {
                info!(?report, "Process owners штатно завершены");
            }
        }

        // До этой точки timeout не может дойти: `process::exit` не запускает Drop.
        self.playlist_runtime.suspend_app_state_binding();
        self.app_state = None;
        self.renderer = None;
        self.process_lifecycle = AppShellProcessLifecycle::ShutdownCompleted;
    }

    /// Bounded-завершает AppState, который ещё не был установлен в shell.
    fn shutdown_uninstalled_app_state_or_exit(app_state: &mut AppState) {
        let player = app_state
            .shutdown_player_until(ShutdownDeadline::after(PROCESS_TERMINAL_SHUTDOWN_BUDGET));
        let disposition = player_disposition(player);
        match disposition {
            OwnerTerminalDisposition::ExitRequired => {
                tracing::error!(
                    ?player,
                    "AppState process owner не завершился после ошибки renderer construction"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(
                    ?player,
                    "AppState process owner завершился terminal failure после construction error"
                );
            }
            OwnerTerminalDisposition::Completed => {}
        }
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
                tracing::error!(?outcome, "Local-file owner не завершился bounded");
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(?outcome, "Local-file owner завершился panic");
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
                .is_some_and(AppState::has_pending_prepared_media_strong)
            || self
                .app_state
                .as_ref()
                .is_some_and(AppState::has_pending_local_file_open)
            || self.settings_runtime.has_pending_options_refresh()
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
        let desktop_changed = match (self.app_state.as_mut(), self.renderer.as_ref()) {
            (Some(app_state), Some(renderer)) => {
                let snapshot = app_state.refresh_player_snapshot();
                let changed = crate::transport_runtime::apply_desktop_commands(
                    app_state,
                    &mut self.playlist_runtime,
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
                tracing::error!(error = %error, "Не удалось применить playlist persistence event");
                false
            }
        };
        let discovery_changed = self.playlist_runtime.drain_playlist_discovery();
        let resume_changed = match (self.app_state.as_mut(), self.renderer.as_ref()) {
            (Some(app_state), Some(renderer)) => {
                app_state.drive_suspended_media_resume(&mut self.playlist_runtime, renderer)
            }
            _ => false,
        };
        let startup_changed = match (self.app_state.as_mut(), self.renderer.as_ref()) {
            (Some(app_state), Some(renderer)) => self.startup_media.poll_startup_jobs(
                app_state,
                &mut self.playlist_runtime,
                renderer,
            ),
            _ => false,
        };
        let _resume_status = self.playlist_runtime.suspended_media_status();
        desktop_changed
            || persistence_changed
            || discovery_changed
            || resume_changed
            || startup_changed
    }
}
