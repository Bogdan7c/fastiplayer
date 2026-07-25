//! Process-lifetime startup winner state без filesystem/player ownership.
//!
//! Jobs готовят media параллельно state inspection. Этот модуль хранит результат
//! до allocator gate и не выдаёт Item ID, dirty revision или desktop signal.

use std::path::PathBuf;
use std::sync::Arc;

use player_core::PlaybackIntent;

use crate::local_file_open::LocalFileOpenResult;
use crate::media_open::{ActiveMediaSource, PreparedLocalOpenResult, SafeMediaLabel};
use crate::playlist_runtime::StartupRestoreTarget;
use crate::state::PreparedSingleMediaOpen;
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

use super::{PreparedYtDlpStartupMedia, StartupMediaController};

/// Чья подготовка сейчас владеет единственным startup media slot-ом.
pub(super) enum StartupMediaTarget {
    CliReplacement,
    RestoredCurrent(StartupRestoreTarget),
}

#[cfg(test)]
mod pending_work_tests {
    use super::*;

    /// Scheduler продолжает polling во всех loading/applying фазах и останавливается на terminal.
    #[test]
    fn startup_phases_report_pending_work_without_continuous_playback() {
        let mut orchestration = StartupMediaOrchestration::new(false);

        for pending_phase in [
            StartupMediaPhase::WaitingForRuntime,
            StartupMediaPhase::Preparing,
            StartupMediaPhase::PreparedAwaitingAllocator,
            StartupMediaPhase::Applying,
        ] {
            orchestration.phase = pending_phase;
            assert!(orchestration.has_pending_work());
        }

        for terminal_phase in [
            StartupMediaPhase::Activated,
            StartupMediaPhase::Idle,
            StartupMediaPhase::Failed,
            StartupMediaPhase::Shutdown,
        ] {
            orchestration.phase = terminal_phase;
            assert!(!orchestration.has_pending_work());
        }
    }
}

/// Prepared ownership сохраняется до trusted allocator decision.
pub(super) enum PreparedStartupMedia {
    Local(Box<PreparedLocalOpenResult>),
    YtDlp {
        source_locator: service_ytdlp::YtDlpMediaLocator,
        prepared: Box<PreparedYtDlpStartupMedia>,
    },
    Direct {
        source_locator: service_direct_media::DirectMediaUrl,
        prepared_media: player_core::PreparedMedia,
    },
}

/// Read-only phase нужна scheduler/tests и не раскрывает locator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupMediaPhase {
    WaitingForRuntime,
    Preparing,
    PreparedAwaitingAllocator,
    Applying,
    Activated,
    Idle,
    Failed,
    Shutdown,
}

pub(super) struct StartupMediaOrchestration {
    pub(super) phase: StartupMediaPhase,
    pub(super) target: Option<StartupMediaTarget>,
    pub(super) prepared: Option<PreparedStartupMedia>,
    pub(super) cli_requested: bool,
    pub(super) cli_failed: bool,
    pub(super) restored_fallback_started: bool,
    pub(super) sensitive_cli_persistence_warning: bool,
    pub(super) expected_restore_generation: Option<crate::playlist_runtime::RestoreApplyGeneration>,
    /// Process owner помнит winner policy, пока renderer-bound transaction живёт в `AppState`.
    pub(super) pending_install: Option<StartupPendingInstall>,
}

/// Metadata startup winner-а, не владеющая renderer/player receipts.
pub(super) struct StartupPendingInstall {
    /// CLI failure может открыть сохранённый fallback только для proven pre-barrier terminal.
    pub(super) is_cli: bool,
    /// Только успешный CLI local target запускает sibling discovery после domain commit-а.
    pub(super) local_discovery: Option<(PathBuf, playlist_discovery::LocalMediaKind)>,
    /// Newer user/CLI mutation wins observable startup result while old receipts are drained.
    pub(super) superseded: bool,
}

impl StartupMediaOrchestration {
    pub(super) const fn new(cli_requested: bool) -> Self {
        Self {
            phase: StartupMediaPhase::WaitingForRuntime,
            target: None,
            prepared: None,
            cli_requested,
            cli_failed: false,
            restored_fallback_started: false,
            sensitive_cli_persistence_warning: false,
            expected_restore_generation: None,
            pending_install: None,
        }
    }

    pub(super) fn begin_target(&mut self, target: StartupMediaTarget) {
        self.target = Some(target);
        self.prepared = None;
        self.phase = StartupMediaPhase::Preparing;
    }

    pub(super) fn hold_prepared(&mut self, prepared: PreparedStartupMedia, gate_open: bool) {
        self.prepared = Some(prepared);
        self.phase = if gate_open {
            StartupMediaPhase::Applying
        } else {
            StartupMediaPhase::PreparedAwaitingAllocator
        };
    }

    pub(super) fn preparation_failed(&mut self) {
        self.prepared = None;
        self.cli_failed |= matches!(self.target, Some(StartupMediaTarget::CliReplacement));
        self.phase = StartupMediaPhase::Failed;
    }

    pub(super) const fn has_pending_work(&self) -> bool {
        matches!(
            self.phase,
            StartupMediaPhase::WaitingForRuntime
                | StartupMediaPhase::Preparing
                | StartupMediaPhase::PreparedAwaitingAllocator
                | StartupMediaPhase::Applying
        )
    }
}

impl StartupMediaController {
    /// Drains preparation owners, затем единожды применяет winner после allocator gate.
    pub(super) fn drive_startup_orchestration(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        if self.terminal_shutdown_started {
            self.orchestration.phase = StartupMediaPhase::Shutdown;
            return false;
        }
        let mut changed = self.drain_preparation_jobs(app_state, playlist_runtime, renderer);
        changed |= playlist_runtime.drain_owner_mailbox();
        let gate_open = playlist_runtime.allocator_load_gate_is_open();
        let structurally_superseded =
            self.orchestration
                .expected_restore_generation
                .is_some_and(|expected| {
                    expected != playlist_runtime.playlist_startup_view().restore_generation
                })
                || playlist_runtime.startup_media_apply_was_superseded();
        if let Some(playlist_changed) = self.drive_startup_playlist_import(
            app_state,
            playlist_runtime,
            renderer,
            structurally_superseded,
        ) {
            return changed || playlist_changed;
        }
        if let Some(pending_install) = self.orchestration.pending_install.as_mut() {
            if structurally_superseded && !pending_install.superseded {
                pending_install.superseded = true;
                if let Err(error) =
                    app_state.supersede_pending_prepared_media_strong(playlist_runtime)
                {
                    self.startup_error = Some(error.to_string());
                    app_state.set_startup_error(error.to_string());
                }
                changed = true;
            }
            return self.poll_pending_install(app_state, playlist_runtime) || changed;
        }

        if gate_open
            && structurally_superseded
            && self.yt_dlp_startup_job.is_none()
            && self.direct_media_startup_job.is_none()
            && self.local_startup_job.is_none()
        {
            self.orchestration.prepared = None;
            self.orchestration.target = None;
            self.orchestration.phase = StartupMediaPhase::Idle;
            return true;
        }

        if gate_open && !structurally_superseded && self.orchestration.prepared.is_some() {
            self.orchestration.phase = StartupMediaPhase::Applying;
            changed |= self.begin_prepared_winner(app_state, playlist_runtime, renderer);
        }

        if gate_open
            && self.orchestration.prepared.is_none()
            && self.yt_dlp_startup_job.is_none()
            && self.direct_media_startup_job.is_none()
            && self.local_startup_job.is_none()
            && (!self.orchestration.cli_requested || self.orchestration.cli_failed)
            && !structurally_superseded
            && !self.orchestration.restored_fallback_started
        {
            self.orchestration.restored_fallback_started = true;
            changed |= self.start_restored_fallback(app_state, playlist_runtime);
        }
        changed
    }

    fn drain_preparation_jobs(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let mut changed = false;
        if let Some(job) = self.local_startup_job.as_mut() {
            let drain = job.drain();
            changed |= drain.has_payload();
            if let Some(completion) = drain.completion {
                self.local_startup_job = None;
                match completion {
                    LocalFileOpenResult::Prepared { prepared } => {
                        self.hold_prepared(PreparedStartupMedia::Local(prepared), playlist_runtime);
                    }
                    LocalFileOpenResult::PrepareFailed { error, .. }
                    | LocalFileOpenResult::JobFailed { error } => {
                        self.handle_preparation_failure(error, app_state, playlist_runtime);
                    }
                    LocalFileOpenResult::Cancelled => {
                        self.handle_preparation_failure(
                            "Startup local preparation отменена".to_owned(),
                            app_state,
                            playlist_runtime,
                        );
                    }
                    LocalFileOpenResult::Selected { .. } => {
                        self.handle_preparation_failure(
                            "Startup local owner получил неожиданный picker result".to_owned(),
                            app_state,
                            playlist_runtime,
                        );
                    }
                }
            }
        }

        if let Some(job) = self.yt_dlp_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            let source_locator = job.source_locator.clone();
            self.yt_dlp_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(
                    PreparedStartupMedia::YtDlp {
                        source_locator,
                        prepared: Box::new(prepared),
                    },
                    playlist_runtime,
                ),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.direct_media_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            let source_locator = job.source_locator.clone();
            self.direct_media_startup_job = None;
            changed = true;
            match result {
                Ok(opened_media) => {
                    let source_label = opened_media.source_label().to_owned();
                    let prepared_media = player_core::PreparedMedia::from_external_label(
                        source_label.clone(),
                        opened_media.into_demuxer(),
                    );
                    self.hold_prepared(
                        PreparedStartupMedia::Direct {
                            source_locator,
                            prepared_media,
                        },
                        playlist_runtime,
                    );
                }
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if playlist_runtime.allocator_load_gate_is_open() && self.orchestration.prepared.is_some() {
            changed |= self.begin_prepared_winner(app_state, playlist_runtime, renderer);
        }
        changed
    }

    fn hold_prepared(
        &mut self,
        prepared: PreparedStartupMedia,
        playlist_runtime: &crate::playlist_runtime::PlaylistRuntime,
    ) {
        self.startup_error = None;
        self.orchestration
            .hold_prepared(prepared, playlist_runtime.allocator_load_gate_is_open());
    }

    pub(super) fn handle_preparation_failure(
        &mut self,
        safe_error: String,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        let target = self.orchestration.target.take();
        self.orchestration.preparation_failed();
        self.startup_error = Some(safe_error.clone());
        app_state.set_startup_error(safe_error.clone());

        if let Some(StartupMediaTarget::RestoredCurrent(failed)) = target {
            let next = playlist_runtime
                .report_startup_restore_failure(failed, Arc::<str>::from(safe_error));
            if let Some(next) = next {
                self.start_restored_target(next, app_state, playlist_runtime);
            }
        }
    }

    fn start_restored_fallback(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) -> bool {
        let Some(target) = playlist_runtime.startup_restored_current() else {
            self.orchestration.phase = StartupMediaPhase::Idle;
            return true;
        };
        self.start_restored_target(target, app_state, playlist_runtime);
        true
    }

    pub(super) fn start_restored_target(
        &mut self,
        mut target: StartupRestoreTarget,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        loop {
            let locator = target.locator.clone();
            self.orchestration
                .begin_target(StartupMediaTarget::RestoredCurrent(target));
            if let Some(local_locator) = locator.as_local() {
                if let Some(path) = local_locator.expose_native_path_for_open() {
                    let Some(config) = self.startup_config.as_ref() else {
                        self.handle_preparation_failure(
                            "Startup config недоступен для restored local media".to_owned(),
                            app_state,
                            playlist_runtime,
                        );
                        return;
                    };
                    match crate::local_file_open::LocalFileOpenJob::spawn_preparation(
                        path.to_path_buf(),
                        config.player.demux,
                        self.wake_port.clone(),
                    ) {
                        Ok(job) => {
                            self.local_startup_job = Some(job);
                            app_state.set_startup_pending(
                                "Восстановление media в состоянии Pause...".to_owned(),
                            );
                            return;
                        }
                        Err(error) => {
                            let failed = self.orchestration.target.take();
                            self.orchestration.preparation_failed();
                            let Some(StartupMediaTarget::RestoredCurrent(failed)) = failed else {
                                return;
                            };
                            let Some(next) = playlist_runtime
                                .report_startup_restore_failure(failed, Arc::<str>::from(error))
                            else {
                                return;
                            };
                            target = next;
                            continue;
                        }
                    }
                }
            } else if let Some(url_locator) = locator.as_secret_url() {
                let service_locator = match classify_playlist_url(url_locator) {
                    StartupUrlClassification::Supported(locator) => locator,
                    StartupUrlClassification::NotUrl
                    | StartupUrlClassification::Unsupported { .. } => {
                        let failed = self.orchestration.target.take();
                        self.orchestration.preparation_failed();
                        let Some(StartupMediaTarget::RestoredCurrent(failed)) = failed else {
                            return;
                        };
                        let Some(next) = playlist_runtime.report_startup_restore_failure(
                            failed,
                            Arc::<str>::from("Persisted URL больше не поддерживается"),
                        ) else {
                            return;
                        };
                        target = next;
                        continue;
                    }
                };
                let Some(config) = self.startup_config.clone() else {
                    return;
                };
                let Some(capabilities) = self.system_capabilities.clone() else {
                    return;
                };
                service_locator.start(self, app_state, &config, &capabilities);
                if self.yt_dlp_startup_job.is_some() || self.direct_media_startup_job.is_some() {
                    return;
                }
                let failed = self.orchestration.target.take();
                let Some(StartupMediaTarget::RestoredCurrent(failed)) = failed else {
                    return;
                };
                let Some(next) = playlist_runtime.report_startup_restore_failure(
                    failed,
                    Arc::<str>::from("Не удалось запустить restored URL preparation"),
                ) else {
                    return;
                };
                target = next;
                continue;
            }

            let failed = self.orchestration.target.take();
            self.orchestration.preparation_failed();
            let Some(StartupMediaTarget::RestoredCurrent(failed)) = failed else {
                return;
            };
            let Some(next) = playlist_runtime.report_startup_restore_failure(
                failed,
                Arc::<str>::from("Persisted local path недоступен на этой platform"),
            ) else {
                return;
            };
            target = next;
        }
    }

    fn begin_prepared_winner(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let Some(prepared) = self.orchestration.prepared.take() else {
            return false;
        };
        let Some(target) = self.orchestration.target.take() else {
            self.orchestration.phase = StartupMediaPhase::Failed;
            return true;
        };
        let is_cli = matches!(&target, StartupMediaTarget::CliReplacement);
        let autoplay = self
            .startup_config
            .as_ref()
            .is_some_and(|config| !config.player.start_paused);
        let mut pending_install = None;

        let install_result = match prepared {
            PreparedStartupMedia::Local(prepared) => {
                let path = prepared.source_path.clone();
                let media_kind = prepared.media_kind;
                let source = ActiveMediaSource::LocalFile(path.clone());
                let input = match target {
                    StartupMediaTarget::CliReplacement => {
                        let target_draft =
                            match crate::playlist_runtime::discovery::target_draft_from_prepared(
                                &prepared,
                            ) {
                                Ok(target_draft) => target_draft,
                                Err(error) => {
                                    self.handle_install_failure(
                                        error.to_string(),
                                        is_cli,
                                        app_state,
                                    );
                                    return true;
                                }
                            };
                        PreparedSingleMediaOpen::target_replacement(
                            prepared.prepared_media,
                            source.clone(),
                            prepared.safe_label,
                            target_draft,
                        )
                    }
                    StartupMediaTarget::RestoredCurrent(target) => {
                        PreparedSingleMediaOpen::restored_current(
                            prepared.prepared_media,
                            source.clone(),
                            prepared.safe_label,
                            target,
                        )
                    }
                };
                let intent = if is_cli {
                    PlaybackIntent::from_autoplay(autoplay)
                } else {
                    PlaybackIntent::StartPaused
                };
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: is_cli.then_some((path, media_kind)),
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::YtDlp {
                source_locator,
                prepared,
            } => {
                let prepared = *prepared;
                let source = ActiveMediaSource::YtDlpUrl {
                    source_locator: source_locator.clone(),
                    candidate_selection: Box::new(prepared.candidate_selection),
                    stream_configuration: Box::new(prepared.stream_configuration),
                };
                let mut prepared_media = player_core::PreparedMedia::from_external_label(
                    source_locator.safe_label(),
                    prepared.demuxer,
                );
                if let Some(port) = prepared.demux_seek_port {
                    prepared_media = prepared_media.with_worker_receipted_demux_seek(port);
                }
                if let Some(window) = prepared.playback_window {
                    prepared_media = match prepared_media.with_playback_window(window) {
                        Ok(prepared_media) => prepared_media,
                        Err(error) => {
                            self.handle_install_failure(error.to_string(), is_cli, app_state);
                            return true;
                        }
                    };
                }
                if let Some(timeline_port) = prepared.timeline_port {
                    prepared_media = match prepared_media.with_dynamic_timeline(timeline_port) {
                        Ok(prepared_media) => prepared_media,
                        Err(error) => {
                            self.handle_install_failure(error.to_string(), is_cli, app_state);
                            return true;
                        }
                    };
                }
                let input = self.prepared_url_input(
                    prepared_media,
                    source.clone(),
                    SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
                    target,
                );
                app_state
                    .begin_prepared_media_strong(
                        playlist_runtime,
                        renderer,
                        input,
                        if is_cli {
                            PlaybackIntent::from_autoplay(autoplay)
                        } else {
                            PlaybackIntent::StartPaused
                        },
                    )
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::Direct {
                source_locator,
                prepared_media,
            } => {
                let source = ActiveMediaSource::DirectMediaUrl(source_locator.clone());
                let input = self.prepared_url_input(
                    prepared_media,
                    source.clone(),
                    SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
                    target,
                );
                app_state
                    .begin_prepared_media_strong(
                        playlist_runtime,
                        renderer,
                        input,
                        if is_cli {
                            PlaybackIntent::from_autoplay(autoplay)
                        } else {
                            PlaybackIntent::StartPaused
                        },
                    )
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
        };

        match install_result {
            Ok(()) => {
                self.startup_error = None;
                playlist_runtime.begin_startup_action_retention();
                self.orchestration.pending_install = pending_install;
                self.orchestration.phase = StartupMediaPhase::Applying;
            }
            Err(error) => {
                let safe_error = error.to_string();
                if !is_cli
                    && let Some(request_id) = error.terminal_request_id()
                    && let Some(next) = playlist_runtime.report_startup_restore_install_failure(
                        request_id,
                        Arc::<str>::from(safe_error.clone()),
                    )
                {
                    self.start_restored_target(next, app_state, playlist_runtime);
                    return true;
                }
                self.handle_install_failure(safe_error, is_cli, app_state);
            }
        }
        true
    }

    fn prepared_url_input(
        &mut self,
        prepared_media: player_core::PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
        target: StartupMediaTarget,
    ) -> PreparedSingleMediaOpen {
        match target {
            StartupMediaTarget::CliReplacement => PreparedSingleMediaOpen::target_replacement(
                prepared_media,
                source,
                safe_label,
                self.cli_url_target_draft
                    .take()
                    .expect("CLI URL preparation keeps its ID-less replacement draft"),
            ),
            StartupMediaTarget::RestoredCurrent(target) => {
                PreparedSingleMediaOpen::restored_current(
                    prepared_media,
                    source,
                    safe_label,
                    target,
                )
            }
        }
    }

    pub(super) fn handle_install_failure(
        &mut self,
        safe_error: String,
        is_cli: bool,
        app_state: &mut crate::state::AppState,
    ) {
        self.startup_error = Some(safe_error.clone());
        app_state.set_startup_error(safe_error);
        self.orchestration.cli_failed |= is_cli;
        self.orchestration.phase = StartupMediaPhase::Failed;
    }
}
