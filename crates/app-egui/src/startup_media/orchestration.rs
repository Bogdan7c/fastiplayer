//! Process-lifetime startup winner state без filesystem/player ownership.
//!
//! Jobs готовят media параллельно state inspection. Этот модуль хранит результат
//! до allocator gate и не выдаёт Item ID, dirty revision или desktop signal.

use std::path::PathBuf;
use std::sync::Arc;

use player_core::PlaybackIntent;

use crate::media_open::{ActiveMediaSource, SafeMediaLabel};
use crate::playlist_runtime::StartupRestoreTarget;
use crate::startup_readiness::StartupMediaOpenKind;
use crate::state::PreparedSingleMediaOpen;
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

use super::StartupMediaController;

/// Чья подготовка сейчас владеет единственным startup media slot-ом.
pub(super) enum StartupMediaTarget {
    CliReplacement,
    RestoredCurrent(StartupRestoreTarget),
}

mod drain;
#[cfg(test)]
#[path = "orchestration/pending_work_tests.rs"]
mod pending_work_tests;
mod prepared;
mod web_preparation;
pub(super) use prepared::PreparedStartupMedia;
pub(crate) use prepared::apply_restored_playback_policy;
use prepared::prepared_startup_audio_proof;

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
            && self.native_hls_startup_job.is_none()
            && self.native_dash_startup_job.is_none()
            && self.native_hds_startup_job.is_none()
            && self.native_smooth_startup_job.is_none()
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
            && self.native_hls_startup_job.is_none()
            && self.native_dash_startup_job.is_none()
            && self.native_hds_startup_job.is_none()
            && self.native_smooth_startup_job.is_none()
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
        let Some(config) = self.startup_config.clone() else {
            // Config absence — process-wide failure, поэтому D22 item fallback здесь бессмысленен.
            self.handle_install_failure(
                "Startup config недоступен для restored media".to_owned(),
                false,
                app_state,
            );
            return;
        };
        loop {
            // Каждый D22 fallback создаётся domain-owner-ом с безопасным paused default.
            // Startup owner повторно применяет пользовательскую policy до нового admission.
            apply_restored_playback_policy(&mut target, &config);
            let locator = target.locator.clone();
            let readiness_target = match target.position() {
                crate::playlist_runtime::StartupPosition::KeepStart => {
                    crate::startup_readiness::StartupTargetExpectation::Beginning
                }
                crate::playlist_runtime::StartupPosition::Restore(target_position) => {
                    crate::startup_readiness::StartupTargetExpectation::Restore { target_position }
                }
            };
            let readiness_playback = match target.playback_intent() {
                player_core::PlaybackIntent::StartPlaying => {
                    crate::startup_readiness::StartupPlaybackExpectation::Playing
                }
                player_core::PlaybackIntent::StartPaused => {
                    crate::startup_readiness::StartupPlaybackExpectation::Paused
                }
            };
            self.orchestration
                .begin_target(StartupMediaTarget::RestoredCurrent(target));
            app_state.begin_startup_readiness(
                crate::startup_readiness::StartupReadinessExpectation::new(
                    StartupMediaOpenKind::Restore,
                    readiness_target,
                    readiness_playback,
                    crate::startup_readiness::StartupAudioExpectation::Unknown,
                ),
            );
            if let Some(local_locator) = locator.as_local() {
                if let Some(path) = local_locator.expose_native_path_for_open() {
                    match crate::local_file_open::LocalFileOpenJob::spawn_preparation(
                        path.to_path_buf(),
                        config.player.demux,
                        self.wake_port.clone(),
                    ) {
                        Ok(job) => {
                            self.local_startup_job = Some(job);
                            app_state.set_startup_pending("Восстановление media...".to_owned());
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
                let Some(capabilities) = self.system_capabilities.clone() else {
                    self.handle_install_failure(
                        "System capabilities недоступны для restored URL".to_owned(),
                        false,
                        app_state,
                    );
                    return;
                };
                service_locator.start(self, app_state, &config, &capabilities);
                // URL adapter считается успешно запущенным только пока один из
                // трёх mutually-exclusive owner jobs действительно удерживается
                // controller-ом. Native HLS — такой же полноценный startup owner,
                // а не промежуточный probe перед yt-dlp fallback.
                if self.yt_dlp_startup_job.is_some()
                    || self.direct_media_startup_job.is_some()
                    || self.native_hls_startup_job.is_some()
                    || self.native_dash_startup_job.is_some()
                    || self.native_hds_startup_job.is_some()
                    || self.native_smooth_startup_job.is_some()
                {
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
        let playback_intent = match &target {
            StartupMediaTarget::CliReplacement => PlaybackIntent::from_autoplay(autoplay),
            StartupMediaTarget::RestoredCurrent(target) => target.playback_intent(),
        };
        let mut pending_install = None;

        let install_result = match prepared {
            PreparedStartupMedia::Local(prepared) => {
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    &prepared.tracks,
                ));
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
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: is_cli.then_some((path, media_kind)),
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::Extractor {
                source_locator,
                prepared,
            } => {
                let prepared = *prepared;
                let tracks = prepared.demuxer.tracks().to_vec();
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(&tracks));
                let demux_duration = prepared.playback_window.and_then(|window| {
                    window
                        .end_exclusive()
                        .and_then(|end| end.as_duration().checked_sub(window.start().as_duration()))
                });
                let demux_duration = demux_duration.or_else(|| prepared.demuxer.duration());
                let demux_metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
                let playlist_duration = crate::media_open::service_duration_for_timeline(
                    prepared.timeline_port.as_ref(),
                    prepared.playlist_metadata.duration(),
                );
                let (duration, metadata) = crate::media_open::merge_yt_dlp_playlist_metadata(
                    demux_duration,
                    demux_metadata,
                    prepared.playlist_metadata.title(),
                    playlist_duration,
                );
                let safe_label =
                    SafeMediaLabel::from_service_safe_label(source_locator.safe_label());
                let source_intent = crate::media_open::WebMediaSourceIntent::extractor(
                    source_locator.clone(),
                    prepared.presentation,
                    prepared.source_state,
                    prepared.extractor_reason,
                );
                let source = ActiveMediaSource::Web(source_intent.clone());
                let prepared_media = match crate::media_open::compose_prepared_web_media(
                    source_locator.safe_label(),
                    prepared.demuxer,
                    crate::media_open::PreparedWebMediaAttachments {
                        timeline_port: prepared.timeline_port,
                        demux_seek: prepared.demux_seek_port.map(
                            crate::media_open::PreparedWebMediaSeekAttachment::WorkerReceipted,
                        ),
                        playback_window: prepared.playback_window,
                        initial_position: None,
                    },
                ) {
                    Ok(prepared_media) => prepared_media,
                    Err(error) => {
                        self.handle_install_failure(error.to_string(), is_cli, app_state);
                        return true;
                    }
                };
                let input = self
                    .prepared_url_input(prepared_media, source.clone(), safe_label.clone(), target)
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(
                        crate::media_open::PreparedWebMediaEnvelope::new(
                            tracks,
                            duration,
                            metadata,
                            source_intent,
                            safe_label,
                            prepared.playback_window,
                            prepared.vod_endpoint_recovery,
                        ),
                    ));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
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
                descriptor,
            } => {
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared_media.tracks(),
                ));
                let source = ActiveMediaSource::Web(descriptor.source().clone());
                let input = self
                    .prepared_url_input(
                        prepared_media,
                        source,
                        SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(*descriptor));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::NativeHls { source, prepared } => {
                let prepared =
                    match web_preparation::compose_native_hls_startup_media(source, *prepared) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            self.handle_install_failure(error, is_cli, app_state);
                            return true;
                        }
                    };
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared.prepared_media.tracks(),
                ));
                let input = self
                    .prepared_url_input(
                        prepared.prepared_media,
                        prepared.active_source,
                        prepared.safe_label,
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(
                        prepared.descriptor,
                    ));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::NativeDash { source, prepared } => {
                let prepared =
                    match web_preparation::compose_native_dash_startup_media(source, *prepared) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            self.handle_install_failure(error, is_cli, app_state);
                            return true;
                        }
                    };
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared.prepared_media.tracks(),
                ));
                let input = self
                    .prepared_url_input(
                        prepared.prepared_media,
                        prepared.active_source,
                        prepared.safe_label,
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(
                        prepared.descriptor,
                    ));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::NativeHds { source, prepared } => {
                let prepared =
                    match web_preparation::compose_native_hds_startup_media(source, *prepared) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            self.handle_install_failure(error, is_cli, app_state);
                            return true;
                        }
                    };
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared.prepared_media.tracks(),
                ));
                let input = self
                    .prepared_url_input(
                        prepared.prepared_media,
                        prepared.active_source,
                        prepared.safe_label,
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(
                        prepared.descriptor,
                    ));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
                    .map(|_| {
                        pending_install = Some(StartupPendingInstall {
                            is_cli,
                            local_discovery: None,
                            superseded: false,
                        });
                    })
            }
            PreparedStartupMedia::NativeSmooth { source, prepared } => {
                let prepared =
                    match web_preparation::compose_native_smooth_startup_media(source, *prepared) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            self.handle_install_failure(error, is_cli, app_state);
                            return true;
                        }
                    };
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared.prepared_media.tracks(),
                ));
                let input = self
                    .prepared_url_input(
                        prepared.prepared_media,
                        prepared.active_source,
                        prepared.safe_label,
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::Web(
                        prepared.descriptor,
                    ));
                app_state
                    .begin_prepared_media_strong(playlist_runtime, renderer, input, playback_intent)
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
        app_state.abort_startup_readiness(
            crate::startup_readiness::StartupReadinessAbortReason::InstallationFailed,
        );
        app_state.set_startup_error(safe_error);
        self.orchestration.cli_failed |= is_cli;
        self.orchestration.phase = StartupMediaPhase::Failed;
    }
}
