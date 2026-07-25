//! S25/S36 controlled same-item candidate/component switch orchestration.

use player_core::MediaInstallCancellationCause;
use render_wgpu_shell::Renderer;

use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution,
};
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarPendingSelection, UrlSidebarSafeError, UrlSidebarTransitionError,
    WebMediaSelectionPreference, WebMediaStreamGeneration,
};

use super::{ActiveMediaSource, AppState, StrongMediaOpenError, StrongMediaOpenPoll};

/// Renderer-bound correlation поверх policy-neutral media-open request-а.
pub(super) struct PendingSameItemSwitch {
    /// Exact request нужен cancel/shutdown и terminal correlation.
    request_id: MediaOpenRequestId,
    /// Exact app lineage остаётся неизменной после нового Installed instance-а.
    expected_active: ActiveMediaIdentity,
    /// Вид switch-а определяет единственное допустимое post-Installed изменение.
    kind: SameItemSwitchKind,
}

/// Post-Installed semantics общего strong reopen без positional bool.
enum SameItemSwitchKind {
    /// Candidate completion публикует runtime-only item height override.
    Candidate {
        parent_generation: WebMediaStreamGeneration,
        candidate: crate::web_media_stream_model::WebMediaCandidatePresentation,
        preferred_height: Option<u32>,
    },
    /// Component completion сохраняет существующий preference/override без изменений.
    Component(crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction),
}

impl SameItemSwitchKind {
    /// Строит единственную safe pending projection из authoritative switch kind.
    fn pending_selection(&self) -> UrlSidebarPendingSelection {
        match self {
            Self::Candidate {
                parent_generation,
                candidate,
                ..
            } => UrlSidebarPendingSelection::Candidate {
                parent_generation: *parent_generation,
                candidate: candidate.clone(),
            },
            Self::Component(action) => UrlSidebarPendingSelection::Component(*action),
        }
    }

    /// Возвращает generation родительской installed stream configuration.
    const fn parent_generation(&self) -> WebMediaStreamGeneration {
        match self {
            Self::Candidate {
                parent_generation, ..
            } => *parent_generation,
            Self::Component(action) => action.parent_generation(),
        }
    }
}

/// Явный результат применения safe UI action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarActionApplyOutcome {
    /// Controlled same-lineage reopen запущен.
    Started,
    /// Component row уже активна; lifecycle и UI error state не менялись.
    NoChange,
}

/// Typed start failure не смешивает stale UI, lifecycle и media-open busy.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SameItemSwitchError {
    /// В процессе уже находится одна controlled switch транзакция.
    #[error("same-item media switch уже выполняется")]
    Busy,
    /// UI intent больше не соответствует installed source generation.
    #[error("same-item media switch intent устарел")]
    Stale,
    /// Component action не прошёл generation/axis/index validation владельца catalog-а.
    #[error(transparent)]
    ComponentAction(#[from] ComponentVariantActionError),
    /// Active source не является переключаемым YtDlp source.
    #[error("active source не поддерживает same-item media switch")]
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
    #[error("player runtime preflight отклонил same-item media switch: {0}")]
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
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let generation = action.parent_generation();
        let result = self.start_same_item_switch(action, playlist_runtime, renderer);
        if let Err(error) = &result {
            let _reported = self
                .url_sidebar_controller
                .record_switch_start_rejected(generation, safe_error_for_switch_error(error));
        }
        result
    }

    /// Выполняет start preflight без побочного форматирования UI error-а.
    fn start_same_item_switch(
        &mut self,
        action: UrlSidebarAction,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        if self.same_item_switch.is_some() {
            return Err(SameItemSwitchError::Busy);
        }
        match self.runtime_reconfigure_boundary_activity() {
            Ok(None) => {}
            Ok(Some(_)) => return Err(SameItemSwitchError::Busy),
            Err(error) => return Err(SameItemSwitchError::RuntimePreflight(error)),
        }
        let active_source = self
            .active_media_source
            .clone()
            .ok_or(SameItemSwitchError::UnsupportedSource)?;
        let (source_locator, selection_intent, kind) = match active_source.physical_source() {
            ActiveMediaSource::YtDlpUrl {
                source_locator,
                stream_configuration,
                ..
            } => match action {
                UrlSidebarAction::SelectCandidate {
                    generation,
                    candidate_index,
                } => {
                    let candidate_selection = stream_configuration
                        .candidate_selection_for_switch(generation, candidate_index)
                        .ok_or(SameItemSwitchError::Stale)?;
                    let candidate_presentation = stream_configuration
                        .candidates()
                        .get(candidate_index)
                        .cloned()
                        .ok_or(SameItemSwitchError::Stale)?;
                    if candidate_presentation == *stream_configuration.active_candidate() {
                        return Err(SameItemSwitchError::Stale);
                    }
                    let preferred_height = candidate_presentation.height;
                    (
                        source_locator.clone(),
                        crate::web_media_open::YtDlpCandidateOpenIntent::exact_parent_provider_default(
                            Box::new(candidate_selection),
                            WebMediaSelectionPreference::ItemOverride(preferred_height),
                        ),
                        SameItemSwitchKind::Candidate {
                            parent_generation: generation,
                            candidate: candidate_presentation,
                            preferred_height,
                        },
                    )
                }
                UrlSidebarAction::SelectComponentVariant(component_action) => {
                    let semantic_selection = match stream_configuration
                        .resolve_component_variant_action(component_action)?
                    {
                        ComponentVariantActionResolution::NoChange => {
                            return Ok(UrlSidebarActionApplyOutcome::NoChange);
                        }
                        ComponentVariantActionResolution::SemanticReopen(selection) => selection,
                    };
                    let active_candidate_selection = stream_configuration
                        .active_candidate_selection_for_component_switch()
                        .ok_or(SameItemSwitchError::Stale)?;
                    (
                            source_locator.clone(),
                            crate::web_media_open::YtDlpCandidateOpenIntent::exact_with_component_semantic_selection(
                                Box::new(active_candidate_selection),
                                stream_configuration,
                                semantic_selection,
                            ),
                            SameItemSwitchKind::Component(component_action),
                        )
                }
            },
            ActiveMediaSource::LocalFile(_)
            | ActiveMediaSource::DirectMediaUrl(_)
            | ActiveMediaSource::PlaybackWindow { .. } => {
                return Err(SameItemSwitchError::UnsupportedSource);
            }
        };
        let expected_active = playlist_runtime
            .playlist_view_snapshot()
            .active_media()
            .ok_or(SameItemSwitchError::MissingActiveIdentity)?;
        let binding = playlist_runtime
            .current_binding()
            .ok_or(SameItemSwitchError::MissingBinding)?;
        if expected_active.player_binding_generation() != binding.binding_generation() {
            return Err(SameItemSwitchError::MissingActiveIdentity);
        }
        let playback_snapshot = self.refresh_player_snapshot();
        if playback_snapshot.media_instance_id != Some(expected_active.media_instance_id()) {
            return Err(SameItemSwitchError::MissingActiveIdentity);
        }
        let config = self.committed_app_config();
        let capabilities = self
            .system_capabilities_snapshot
            .clone()
            .ok_or(SameItemSwitchError::MissingCapabilities)?;
        let physical_request = MediaOpenSourceRequest::YtDlp {
            locator: source_locator,
            selection_intent,
            network_config: config.network,
            yt_dlp_config: config.yt_dlp,
            demux_config: config.player.demux,
            preferred_video_codec_order: config.player.preferred_video_codec_order,
            system_capabilities: capabilities,
            audio_capabilities: self.audio_decode_capability_snapshot(),
        };
        let source_request = active_source.wrap_reopen_request(physical_request);
        let pending_selection = kind.pending_selection();
        self.url_sidebar_controller
            .record_switch_started(pending_selection.clone())
            .map_err(|UrlSidebarTransitionError::Busy| SameItemSwitchError::Busy)?;
        let request_id = match self.begin_same_lineage_source_media_strong(
            playlist_runtime,
            renderer,
            source_request,
            expected_active,
            super::playback_intent_from_snapshot(&playback_snapshot),
        ) {
            Ok(request_id) => request_id,
            Err(error) => {
                let _cleared = self.url_sidebar_controller.record_switch_failed(
                    &pending_selection,
                    action.parent_generation(),
                    safe_error_for_start_failure(&error),
                );
                return Err(SameItemSwitchError::Strong(error));
            }
        };
        self.same_item_switch = Some(PendingSameItemSwitch {
            request_id,
            expected_active,
            kind,
        });
        self.mark_pending_worker_redraw();
        Ok(UrlSidebarActionApplyOutcome::Started)
    }

    /// Продвигает shared strong envelope и публикует selectors только после exact Installed.
    pub(crate) fn poll_same_item_switch(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        let Some(pending) = self.same_item_switch.take() else {
            return;
        };
        let previous_generation = pending.kind.parent_generation();
        let pending_selection = pending.kind.pending_selection();
        let matching_strong_request = self
            .pending_strong_media_open
            .as_ref()
            .is_some_and(|strong_pending| strong_pending.request_id() == pending.request_id);
        if !matching_strong_request {
            let _cleared = self.url_sidebar_controller.record_switch_terminal_failed(
                &pending_selection,
                previous_generation,
                UrlSidebarSafeError::SameItemSwitchStale,
            );
            tracing::error!(
                request_id = ?pending.request_id,
                "Same-item media switch потерял matching strong request"
            );
            self.mark_pending_worker_redraw();
            return;
        }
        match self.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {
                self.same_item_switch = Some(pending);
            }
            StrongMediaOpenPoll::Installed(installed) => {
                let installed_configuration = installed
                    .source
                    .physical_source()
                    .yt_dlp_stream_configuration();
                let Some(installed_configuration) = installed_configuration else {
                    let _restored = self.url_sidebar_controller.record_switch_terminal_failed(
                        &pending_selection,
                        previous_generation,
                        UrlSidebarSafeError::SameItemSwitchStale,
                    );
                    tracing::error!(
                        request_id = ?pending.request_id,
                        invariant = "installed_source_not_yt_dlp",
                        "Same-item media switch получил Installed source без YtDlp configuration"
                    );
                    self.mark_pending_worker_redraw();
                    return;
                };
                let installed_generation = installed_configuration.generation();
                if !installed_generation.has_same_source_lineage(previous_generation) {
                    let _restored = self.url_sidebar_controller.record_switch_terminal_failed(
                        &pending_selection,
                        installed_generation,
                        UrlSidebarSafeError::SameItemSwitchStale,
                    );
                    tracing::error!(
                        request_id = ?pending.request_id,
                        invariant = "installed_source_lineage_mismatch",
                        "Same-item media switch получил Installed source другой lineage"
                    );
                    self.mark_pending_worker_redraw();
                    return;
                }
                if matches!(pending.kind, SameItemSwitchKind::Component(_))
                    && !matches!(
                        installed_configuration.component_variant_projection(),
                        crate::web_media_stream_model::component_variants::WebMediaComponentVariantProjection::Installed(_)
                    )
                {
                    let _restored = self.url_sidebar_controller.record_switch_terminal_failed(
                        &pending_selection,
                        installed_generation,
                        UrlSidebarSafeError::SameItemSwitchStale,
                    );
                    tracing::error!(
                        request_id = ?pending.request_id,
                        invariant = "component_catalog_not_installed",
                        "Component switch завершился без fresh Installed component catalog"
                    );
                    self.mark_pending_worker_redraw();
                    return;
                }
                match pending.kind {
                    SameItemSwitchKind::Candidate {
                        preferred_height, ..
                    } => {
                        self.url_sidebar_controller
                            .record_candidate_switch_installed(
                                installed_generation,
                                pending.expected_active.item_id(),
                                preferred_height,
                            );
                    }
                    SameItemSwitchKind::Component(_) => {
                        self.url_sidebar_controller
                            .record_component_switch_installed();
                    }
                }
            }
            StrongMediaOpenPoll::Failed(error) => {
                let safe_error = safe_error_for_terminal_failure(&error);
                let visible_generation = self
                    .active_media_source
                    .as_ref()
                    .and_then(ActiveMediaSource::yt_dlp_stream_generation)
                    .unwrap_or(previous_generation);
                let _restored = self.url_sidebar_controller.record_switch_terminal_failed(
                    &pending_selection,
                    visible_generation,
                    safe_error,
                );
                tracing::warn!(
                    request_id = ?pending.request_id,
                    error = %error,
                    "Same-item media switch завершился ошибкой"
                );
            }
        }
        self.mark_pending_worker_redraw();
    }

    /// Suspend отменяет pre-barrier работу либо дренирует commit-winner до rebind-а.
    pub(crate) fn resolve_same_item_switch_for_suspend(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), crate::playlist_runtime::ResumeCheckpointError> {
        let Some(pending) = self.same_item_switch.as_ref() else {
            return Ok(());
        };
        let request_id = pending.request_id;
        playlist_runtime
            .cancel_media_open_lossless(
                request_id,
                MediaInstallCancellationCause::LifecycleSuspended,
            )
            .map_err(|_| crate::playlist_runtime::ResumeCheckpointError::ControllerInvariant)?;
        while self.same_item_switch.is_some() {
            let phase_before_poll = playlist_runtime
                .media_open_snapshot()
                .map(|snapshot| snapshot.phase);
            self.poll_same_item_switch(playlist_runtime);
            if self.same_item_switch.is_none() {
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
    /// Возвращает freshly Installed YtDlp configuration без раскрытия locator-а.
    fn yt_dlp_stream_configuration(
        &self,
    ) -> Option<&crate::web_media_stream_model::WebMediaStreamConfiguration> {
        match self {
            Self::YtDlpUrl {
                stream_configuration,
                ..
            } => Some(stream_configuration),
            Self::PlaybackWindow { source, .. } => source.yt_dlp_stream_configuration(),
            Self::LocalFile(_) | Self::DirectMediaUrl(_) => None,
        }
    }

    /// Извлекает generation только из freshly Installed YtDlp source.
    fn yt_dlp_stream_generation(&self) -> Option<WebMediaStreamGeneration> {
        self.yt_dlp_stream_configuration()
            .map(crate::web_media_stream_model::WebMediaStreamConfiguration::generation)
    }
}

/// Start rejection сохраняет bounded UI vocabulary.
fn safe_error_for_start_failure(error: &StrongMediaOpenError) -> UrlSidebarSafeError {
    match error {
        StrongMediaOpenError::Start(crate::media_open::MediaOpenStartError::Busy) => {
            UrlSidebarSafeError::SameItemSwitchBusy
        }
        StrongMediaOpenError::SameLineageStale => UrlSidebarSafeError::SameItemSwitchStale,
        _ => UrlSidebarSafeError::SourceUnavailable,
    }
}

/// Любая typed start failure получает bounded, generation-scoped UI категорию.
fn safe_error_for_switch_error(error: &SameItemSwitchError) -> UrlSidebarSafeError {
    match error {
        SameItemSwitchError::Busy => UrlSidebarSafeError::SameItemSwitchBusy,
        SameItemSwitchError::Stale
        | SameItemSwitchError::MissingActiveIdentity
        | SameItemSwitchError::ComponentAction(_) => UrlSidebarSafeError::SameItemSwitchStale,
        SameItemSwitchError::Strong(error) => safe_error_for_start_failure(error),
        SameItemSwitchError::UnsupportedSource
        | SameItemSwitchError::MissingBinding
        | SameItemSwitchError::MissingCapabilities
        | SameItemSwitchError::RuntimePreflight(_) => UrlSidebarSafeError::SourceUnavailable,
    }
}

/// Terminal failure не переносит произвольную error chain в sidebar model.
fn safe_error_for_terminal_failure(error: &StrongMediaOpenError) -> UrlSidebarSafeError {
    match error {
        StrongMediaOpenError::SameLineageStale => UrlSidebarSafeError::SameItemSwitchStale,
        StrongMediaOpenError::Terminal(
            crate::media_open::MediaOpenTerminalOutcome::Cancelled { .. },
        ) => UrlSidebarSafeError::SameItemSwitchCancelled,
        _ => UrlSidebarSafeError::SourceUnavailable,
    }
}
