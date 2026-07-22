//! S25 controlled same-item candidate switch orchestration.

use player_core::MediaInstallCancellationCause;
use render_wgpu_shell::Renderer;

use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarSafeError, UrlSidebarTransitionError, WebMediaSelectionPreference,
    WebMediaStreamGeneration,
};

use super::{ActiveMediaSource, AppState, StrongMediaOpenError, StrongMediaOpenPoll};

/// Renderer-bound correlation поверх policy-neutral media-open request-а.
pub(super) struct PendingSameItemCandidateSwitch {
    /// Exact request нужен cancel/shutdown и terminal correlation.
    request_id: MediaOpenRequestId,
    /// UI generation защищает selector от stale completion.
    previous_generation: WebMediaStreamGeneration,
    /// Exact app lineage остаётся неизменной после нового Installed instance-а.
    expected_active: ActiveMediaIdentity,
    /// Runtime-only предпочтение выбранного candidate-а.
    preferred_height: Option<u32>,
}

/// Typed start failure не смешивает stale UI, lifecycle и media-open busy.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SameItemCandidateSwitchError {
    /// В процессе уже находится одна controlled switch транзакция.
    #[error("same-item candidate switch уже выполняется")]
    Busy,
    /// UI intent больше не соответствует installed source generation.
    #[error("same-item candidate switch intent устарел")]
    Stale,
    /// Active source не является переключаемым YtDlp source.
    #[error("active source не поддерживает candidate switch")]
    UnsupportedSource,
    /// App/controller больше не подтверждают exact active media identity.
    #[error("active media identity отсутствует либо уже изменена")]
    MissingActiveIdentity,
    /// Renderer-bound playlist binding неактивен.
    #[error("playlist runtime binding недоступен")]
    MissingBinding,
    /// Capability snapshot ещё не опубликован composition owner-ом.
    #[error("system capability snapshot недоступен")]
    MissingCapabilities,
    /// Player lifecycle preflight не удалось прочитать без потери причины.
    #[error("player runtime preflight отклонил candidate switch: {0}")]
    RuntimePreflight(player_core::PlayerRuntimeApplyError),
    /// Общий strong media-open envelope отверг запуск.
    #[error(transparent)]
    Strong(#[from] StrongMediaOpenError),
}

impl AppState {
    /// Валидирует safe UI intent и запускает background exact semantic rematch/open.
    pub(crate) fn apply_url_sidebar_action(
        &mut self,
        action: UrlSidebarAction,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> Result<(), SameItemCandidateSwitchError> {
        let generation = action.generation();
        let result = self.start_same_item_candidate_switch(action, playlist_runtime, renderer);
        if let Err(error) = &result {
            let _reported = self
                .url_sidebar_controller
                .record_candidate_switch_rejected(generation, safe_error_for_switch_error(error));
        }
        result
    }

    /// Выполняет start preflight без побочного форматирования UI error-а.
    fn start_same_item_candidate_switch(
        &mut self,
        action: UrlSidebarAction,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> Result<(), SameItemCandidateSwitchError> {
        if self.same_item_candidate_switch.is_some() {
            return Err(SameItemCandidateSwitchError::Busy);
        }
        match self.runtime_reconfigure_boundary_activity() {
            Ok(None) => {}
            Ok(Some(_)) => return Err(SameItemCandidateSwitchError::Busy),
            Err(error) => return Err(SameItemCandidateSwitchError::RuntimePreflight(error)),
        }
        let UrlSidebarAction::SelectCandidate {
            generation,
            candidate_index,
        } = action;
        let active_source = self
            .active_media_source
            .clone()
            .ok_or(SameItemCandidateSwitchError::UnsupportedSource)?;
        let (source_locator, candidate_selection, candidate_presentation, active_candidate) =
            match active_source.physical_source() {
                ActiveMediaSource::YtDlpUrl {
                    source_locator,
                    stream_configuration,
                    ..
                } => {
                    let candidate_selection = stream_configuration
                        .candidate_selection_for_switch(generation, candidate_index)
                        .ok_or(SameItemCandidateSwitchError::Stale)?;
                    let candidate_presentation = stream_configuration
                        .candidates()
                        .get(candidate_index)
                        .cloned()
                        .ok_or(SameItemCandidateSwitchError::Stale)?;
                    (
                        source_locator.clone(),
                        candidate_selection,
                        candidate_presentation,
                        stream_configuration.active_candidate().clone(),
                    )
                }
                ActiveMediaSource::LocalFile(_)
                | ActiveMediaSource::DirectMediaUrl(_)
                | ActiveMediaSource::PlaybackWindow { .. } => {
                    return Err(SameItemCandidateSwitchError::UnsupportedSource);
                }
            };
        if candidate_presentation == active_candidate {
            return Err(SameItemCandidateSwitchError::Stale);
        }
        let preferred_height = candidate_presentation.height;
        let expected_active = playlist_runtime
            .playlist_view_snapshot()
            .active_media()
            .ok_or(SameItemCandidateSwitchError::MissingActiveIdentity)?;
        let binding = playlist_runtime
            .current_binding()
            .ok_or(SameItemCandidateSwitchError::MissingBinding)?;
        if expected_active.player_binding_generation() != binding.binding_generation() {
            return Err(SameItemCandidateSwitchError::MissingActiveIdentity);
        }
        let playback_snapshot = self.refresh_player_snapshot();
        if playback_snapshot.media_instance_id != Some(expected_active.media_instance_id()) {
            return Err(SameItemCandidateSwitchError::MissingActiveIdentity);
        }
        let config = self.committed_app_config();
        let capabilities = self
            .system_capabilities_snapshot
            .clone()
            .ok_or(SameItemCandidateSwitchError::MissingCapabilities)?;
        let preference = WebMediaSelectionPreference::ItemOverride(preferred_height);
        let physical_request = MediaOpenSourceRequest::YtDlp {
            locator: source_locator,
            selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent::exact(
                Box::new(candidate_selection),
                preference,
            ),
            network_config: config.network,
            yt_dlp_config: config.yt_dlp,
            demux_config: config.player.demux,
            preferred_video_codec_order: config.player.preferred_video_codec_order,
            system_capabilities: capabilities,
            audio_capabilities: self.audio_decode_capability_snapshot(),
        };
        let source_request = active_source.wrap_reopen_request(physical_request);
        self.url_sidebar_controller
            .record_candidate_switch_started(generation, candidate_presentation)
            .map_err(|UrlSidebarTransitionError::Busy| SameItemCandidateSwitchError::Busy)?;
        let request_id = self.begin_same_lineage_source_media_strong(
            playlist_runtime,
            renderer,
            source_request,
            expected_active,
            super::playback_intent_from_snapshot(&playback_snapshot),
        )?;
        self.same_item_candidate_switch = Some(PendingSameItemCandidateSwitch {
            request_id,
            previous_generation: generation,
            expected_active,
            preferred_height,
        });
        self.mark_pending_worker_redraw();
        Ok(())
    }

    /// Продвигает shared strong envelope и публикует selectors только после exact Installed.
    pub(crate) fn poll_same_item_candidate_switch(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) {
        let Some(pending) = self.same_item_candidate_switch.take() else {
            return;
        };
        match self.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {
                self.same_item_candidate_switch = Some(pending);
            }
            StrongMediaOpenPoll::Installed(installed) => {
                let installed_generation = installed
                    .source
                    .physical_source()
                    .yt_dlp_stream_generation();
                let Some(installed_generation) = installed_generation else {
                    let _restored = self.url_sidebar_controller.record_candidate_switch_failed(
                        pending.previous_generation,
                        UrlSidebarSafeError::CandidateSwitchStale,
                    );
                    return;
                };
                let _committed = self
                    .url_sidebar_controller
                    .record_candidate_switch_installed(
                        pending.previous_generation,
                        installed_generation,
                        pending.expected_active.item_id(),
                        pending.preferred_height,
                    );
            }
            StrongMediaOpenPoll::Failed(error) => {
                let safe_error = safe_error_for_terminal_failure(&error);
                let visible_generation = self
                    .active_media_source
                    .as_ref()
                    .and_then(ActiveMediaSource::yt_dlp_stream_generation)
                    .unwrap_or(pending.previous_generation);
                let _restored = self
                    .url_sidebar_controller
                    .record_candidate_switch_rejected(visible_generation, safe_error);
                tracing::warn!(
                    request_id = ?pending.request_id,
                    error = %error,
                    "Same-item candidate switch завершился ошибкой"
                );
            }
        }
        self.mark_pending_worker_redraw();
    }

    /// Suspend отменяет pre-barrier работу либо дренирует commit-winner до rebind-а.
    pub(crate) fn resolve_same_item_candidate_switch_for_suspend(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), crate::playlist_runtime::ResumeCheckpointError> {
        let Some(pending) = self.same_item_candidate_switch.as_ref() else {
            return Ok(());
        };
        let request_id = pending.request_id;
        playlist_runtime
            .cancel_media_open_lossless(
                request_id,
                MediaInstallCancellationCause::LifecycleSuspended,
            )
            .map_err(|_| crate::playlist_runtime::ResumeCheckpointError::ControllerInvariant)?;
        while self.same_item_candidate_switch.is_some() {
            let phase_before_poll = playlist_runtime
                .media_open_snapshot()
                .map(|snapshot| snapshot.phase);
            self.poll_same_item_candidate_switch(playlist_runtime);
            if self.same_item_candidate_switch.is_none() {
                break;
            }
            let Some(snapshot) = playlist_runtime.media_open_snapshot() else {
                std::thread::yield_now();
                continue;
            };
            if phase_before_poll == Some(snapshot.phase)
                && !matches!(
                    snapshot.phase,
                    crate::media_open::MediaOpenPhase::Installed
                        | crate::media_open::MediaOpenPhase::Failed
                )
            {
                playlist_runtime
                    .wait_for_media_open_progress(request_id)
                    .map_err(|_| {
                        crate::playlist_runtime::ResumeCheckpointError::ControllerInvariant
                    })?;
            } else {
                std::thread::yield_now();
            }
        }
        Ok(())
    }
}

impl ActiveMediaSource {
    /// Извлекает generation только из freshly Installed YtDlp source.
    fn yt_dlp_stream_generation(&self) -> Option<WebMediaStreamGeneration> {
        match self {
            Self::YtDlpUrl {
                stream_configuration,
                ..
            } => Some(stream_configuration.generation()),
            Self::PlaybackWindow { source, .. } => source.yt_dlp_stream_generation(),
            Self::LocalFile(_) | Self::DirectMediaUrl(_) => None,
        }
    }
}

/// Start rejection сохраняет bounded UI vocabulary.
fn safe_error_for_start_failure(error: &StrongMediaOpenError) -> UrlSidebarSafeError {
    match error {
        StrongMediaOpenError::Start(crate::media_open::MediaOpenStartError::Busy) => {
            UrlSidebarSafeError::CandidateSwitchBusy
        }
        StrongMediaOpenError::SameLineageStale => UrlSidebarSafeError::CandidateSwitchStale,
        _ => UrlSidebarSafeError::SourceUnavailable,
    }
}

/// Любая typed start failure получает bounded, generation-scoped UI категорию.
fn safe_error_for_switch_error(error: &SameItemCandidateSwitchError) -> UrlSidebarSafeError {
    match error {
        SameItemCandidateSwitchError::Busy => UrlSidebarSafeError::CandidateSwitchBusy,
        SameItemCandidateSwitchError::Stale
        | SameItemCandidateSwitchError::MissingActiveIdentity => {
            UrlSidebarSafeError::CandidateSwitchStale
        }
        SameItemCandidateSwitchError::Strong(error) => safe_error_for_start_failure(error),
        SameItemCandidateSwitchError::UnsupportedSource
        | SameItemCandidateSwitchError::MissingBinding
        | SameItemCandidateSwitchError::MissingCapabilities
        | SameItemCandidateSwitchError::RuntimePreflight(_) => {
            UrlSidebarSafeError::SourceUnavailable
        }
    }
}

/// Terminal failure не переносит произвольную error chain в sidebar model.
fn safe_error_for_terminal_failure(error: &StrongMediaOpenError) -> UrlSidebarSafeError {
    match error {
        StrongMediaOpenError::SameLineageStale => UrlSidebarSafeError::CandidateSwitchStale,
        StrongMediaOpenError::Terminal(
            crate::media_open::MediaOpenTerminalOutcome::Cancelled { .. },
        ) => UrlSidebarSafeError::CandidateSwitchCancelled,
        _ => UrlSidebarSafeError::SourceUnavailable,
    }
}
