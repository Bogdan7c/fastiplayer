//! Process-lifetime startup winner state без filesystem/player ownership.
//!
//! Jobs готовят media параллельно state inspection. Этот модуль хранит результат
//! до allocator gate и не выдаёт Item ID, dirty revision или desktop signal.

use std::path::PathBuf;
use std::sync::Arc;

use player_core::PlaybackIntent;

use crate::local_file_open::LocalFileOpenResult;
use crate::media_open::{
    ActiveMediaSource, PreparedLocalOpenResult, SafeMediaLabel, YtDlpPreparedMediaAttachments,
    prepare_yt_dlp_player_media,
};
use crate::playlist_runtime::StartupRestoreTarget;
use crate::startup_readiness::StartupMediaOpenKind;
use crate::state::PreparedSingleMediaOpen;
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

use super::{PreparedYtDlpStartupMedia, StartupMediaController};

/// Чья подготовка сейчас владеет единственным startup media slot-ом.
pub(super) enum StartupMediaTarget {
    CliReplacement,
    RestoredCurrent(StartupRestoreTarget),
}

/// Применяет актуальную config policy к domain target до strong-open admission.
pub(crate) fn apply_restored_playback_policy(
    target: &mut StartupRestoreTarget,
    config: &rustiplayer_config::AppConfig,
) {
    target.set_playback_intent(PlaybackIntent::from_autoplay(!config.player.start_paused));
}

/// Prepared topology — единственный app-owned источник positive/absent audio proof-а.
fn prepared_startup_audio_proof(
    tracks: &[media_core::TrackInfo],
) -> crate::startup_readiness::StartupAudioProof {
    if tracks
        .iter()
        .any(|track| track.kind == media_core::TrackKind::Audio)
    {
        crate::startup_readiness::StartupAudioProof::Required
    } else {
        crate::startup_readiness::StartupAudioProof::NotPresent
    }
}

#[cfg(test)]
#[path = "orchestration/pending_work_tests.rs"]
mod pending_work_tests;

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
    NativeHls {
        source: crate::media_open::NativeHlsUrl,
        prepared: Box<super::native_hls::PreparedNativeHlsMedia>,
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
            && self.native_hls_startup_job.is_none()
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

        if let Some(job) = self.native_hls_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            self.native_hls_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(prepared, playlist_runtime),
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
            PreparedStartupMedia::YtDlp {
                source_locator,
                prepared,
            } => {
                let prepared = *prepared;
                let tracks = prepared.demuxer.tracks().to_vec();
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(&tracks));
                let duration = prepared.demuxer.duration();
                let metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
                let safe_label =
                    SafeMediaLabel::from_service_safe_label(source_locator.safe_label());
                let source = ActiveMediaSource::YtDlpUrl {
                    source_locator: source_locator.clone(),
                    candidate_selection: Box::new(prepared.candidate_selection),
                    composed_selection: prepared.composed_selection,
                    stream_configuration: Box::new(prepared.stream_configuration),
                    catalog_attachment: prepared.catalog_attachment,
                };
                let prepared_media = match prepare_yt_dlp_player_media(
                    source_locator.safe_label(),
                    prepared.demuxer,
                    YtDlpPreparedMediaAttachments {
                        timeline_port: prepared.timeline_port,
                        demux_seek_port: prepared.demux_seek_port,
                        playback_window: prepared.playback_window,
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
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::YtDlp {
                        tracks,
                        duration,
                        metadata,
                        source: source.clone(),
                        safe_label,
                        vod_endpoint_recovery: prepared.vod_endpoint_recovery,
                    });
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
            } => {
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(
                    prepared_media.tracks(),
                ));
                let source = ActiveMediaSource::DirectMediaUrl(source_locator.clone());
                let input = self.prepared_url_input(
                    prepared_media,
                    source.clone(),
                    SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
                    target,
                );
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
                let super::native_hls::PreparedNativeHlsMedia {
                    demuxer,
                    seek_port,
                    initial_position,
                    selection,
                } = *prepared;
                let tracks = demuxer.tracks().to_vec();
                app_state.note_startup_prepared_audio_proof(prepared_startup_audio_proof(&tracks));
                let duration = demuxer.duration();
                let metadata = demuxer.media_metadata().unwrap_or_default().tags;
                let safe_label = source.safe_label().clone();
                let active_source = ActiveMediaSource::NativeHlsUrl { source, selection };
                let prepared_media = crate::web_media_hls_open::prepare_native_hls_player_media(
                    safe_label.as_str(),
                    crate::web_media_hls_open::PreparedNativeHlsVod {
                        demuxer,
                        seek_port,
                        initial_position,
                    },
                );
                let prepared_media = match prepared_media {
                    Ok(prepared_media) => prepared_media,
                    Err(error) => {
                        self.handle_install_failure(error.to_string(), is_cli, app_state);
                        return true;
                    }
                };
                let input = self
                    .prepared_url_input(
                        prepared_media,
                        active_source.clone(),
                        safe_label.clone(),
                        target,
                    )
                    .with_descriptor(crate::media_open::PreparedMediaDescriptor::NativeHls {
                        tracks,
                        duration,
                        metadata,
                        source: active_source,
                        safe_label,
                    });
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
