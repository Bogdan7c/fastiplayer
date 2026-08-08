//! S25/S36 controlled same-item candidate/component switch orchestration.

mod lifecycle_bridge;

use player_core::MediaInstallCancellationCause;
use render_wgpu_shell::Renderer;

use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
#[cfg(test)]
use crate::web_media_stream_model::WebMediaSelectionPreference;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution,
};
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarPendingSelection, UrlSidebarSafeError, WebMediaStreamGeneration,
};

use super::{ActiveMediaSource, AppState, StrongMediaOpenError, StrongMediaOpenPoll};
use lifecycle_bridge::{
    ProductionSameItemSwitchPollContext, ProductionSameItemSwitchStartContext,
    SameItemSwitchAppPath, SameItemSwitchAppStart,
};

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
    #[cfg(test)]
    Candidate {
        parent_generation: WebMediaStreamGeneration,
        candidate: crate::web_media_stream_model::WebMediaCandidatePresentation,
        preferred_height: Option<u32>,
    },
    /// Component completion сохраняет существующий preference/override без изменений.
    Component(crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction),
    Picker {
        parent_generation: WebMediaStreamGeneration,
        action: crate::web_media_catalog::WebMediaFacetAction,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    },
    AutomaticPicker {
        parent_generation: WebMediaStreamGeneration,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    },
}

impl SameItemSwitchKind {
    /// Строит единственную safe pending projection из authoritative switch kind.
    fn pending_selection(&self) -> UrlSidebarPendingSelection {
        match self {
            #[cfg(test)]
            Self::Candidate {
                parent_generation,
                candidate,
                ..
            } => UrlSidebarPendingSelection::Candidate {
                parent_generation: *parent_generation,
                candidate: candidate.clone(),
            },
            Self::Component(action) => UrlSidebarPendingSelection::Component(*action),
            Self::Picker {
                parent_generation,
                action,
                ..
            } => UrlSidebarPendingSelection::StreamFacet {
                parent_generation: *parent_generation,
                action: *action,
            },
            Self::AutomaticPicker {
                parent_generation, ..
            } => UrlSidebarPendingSelection::AutomaticStreamRestore {
                parent_generation: *parent_generation,
            },
        }
    }

    /// Возвращает generation родительской installed stream configuration.
    const fn parent_generation(&self) -> WebMediaStreamGeneration {
        match self {
            #[cfg(test)]
            Self::Candidate {
                parent_generation, ..
            } => *parent_generation,
            Self::Component(action) => action.parent_generation(),
            Self::Picker {
                parent_generation, ..
            }
            | Self::AutomaticPicker {
                parent_generation, ..
            } => *parent_generation,
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
        let active_source = self.preflight_same_item_switch()?;
        let (source_locator, selection_intent, kind) = match active_source.physical_source() {
            ActiveMediaSource::YtDlpUrl {
                source_locator,
                stream_configuration,
                ..
            } => match action {
                #[cfg(test)]
                UrlSidebarAction::Candidate {
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
                UrlSidebarAction::ComponentVariant(component_action) => {
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
                UrlSidebarAction::StreamFacet {
                    parent_generation,
                    action,
                } => {
                    let crate::web_media_catalog::WebMediaCatalogState::Ready(catalog) =
                        &self.web_media_catalog_state
                    else {
                        return Err(SameItemSwitchError::Stale);
                    };
                    if catalog.parent_generation() != parent_generation {
                        return Err(SameItemSwitchError::Stale);
                    }
                    let target = catalog
                        .resolve_facet_action(action)
                        .cloned()
                        .ok_or(SameItemSwitchError::Stale)?;
                    if target == catalog.active_choice().target {
                        return Ok(UrlSidebarActionApplyOutcome::NoChange);
                    }
                    let selection_intent = picker_selection_intent(&target, stream_configuration)?;
                    (
                        source_locator.clone(),
                        selection_intent,
                        SameItemSwitchKind::Picker {
                            parent_generation,
                            action,
                            target,
                        },
                    )
                }
            },
            ActiveMediaSource::LocalFile(_)
            | ActiveMediaSource::DirectMediaUrl(_)
            | ActiveMediaSource::PlaybackWindow { .. } => {
                return Err(SameItemSwitchError::UnsupportedSource);
            }
        };
        self.start_resolved_same_item_switch(
            active_source,
            source_locator,
            selection_intent,
            kind,
            playlist_runtime,
            renderer,
        )
    }

    pub(super) fn start_automatic_web_media_switch(
        &mut self,
        pending: super::web_media_catalog::PendingAutomaticWebMediaSwitch,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let active_source = self.preflight_same_item_switch()?;
        let ActiveMediaSource::YtDlpUrl {
            source_locator,
            stream_configuration,
            ..
        } = active_source.physical_source()
        else {
            return Err(SameItemSwitchError::UnsupportedSource);
        };
        if stream_configuration.generation() != pending.parent_generation {
            return Err(SameItemSwitchError::Stale);
        }
        let crate::web_media_catalog::WebMediaCatalogState::Ready(catalog) =
            &self.web_media_catalog_state
        else {
            return Err(SameItemSwitchError::Stale);
        };
        if catalog.generation() != pending.catalog_generation
            || catalog.parent_generation() != pending.parent_generation
            || !catalog.contains_target(&pending.target)
        {
            return Err(SameItemSwitchError::Stale);
        }
        if pending.target == catalog.active_choice().target {
            return Ok(UrlSidebarActionApplyOutcome::NoChange);
        }
        let selection_intent = picker_selection_intent(&pending.target, stream_configuration)?;
        let source_locator = source_locator.clone();
        self.start_resolved_same_item_switch(
            active_source,
            source_locator,
            selection_intent,
            SameItemSwitchKind::AutomaticPicker {
                parent_generation: pending.parent_generation,
                target: pending.target,
            },
            playlist_runtime,
            renderer,
        )
    }

    fn preflight_same_item_switch(&mut self) -> Result<ActiveMediaSource, SameItemSwitchError> {
        if self.same_item_switch.is_some() {
            return Err(SameItemSwitchError::Busy);
        }
        match self.runtime_reconfigure_boundary_activity() {
            Ok(None) => {}
            Ok(Some(_)) => return Err(SameItemSwitchError::Busy),
            Err(error) => return Err(SameItemSwitchError::RuntimePreflight(error)),
        }
        self.active_media_source
            .clone()
            .ok_or(SameItemSwitchError::UnsupportedSource)
    }

    fn start_resolved_same_item_switch(
        &mut self,
        active_source: ActiveMediaSource,
        source_locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        kind: SameItemSwitchKind,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
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
            system_capabilities: Box::new(capabilities),
            audio_capabilities: self.audio_decode_capability_snapshot(),
        };
        let source_request = active_source.wrap_reopen_request(physical_request);
        let start = SameItemSwitchAppStart {
            source_request,
            expected_active,
            playback_intent: super::playback_intent_from_snapshot(&playback_snapshot),
            kind,
        };
        let mut app_path = SameItemSwitchAppPath::take(&mut self.same_item_switch);
        let result = {
            let mut lifecycle =
                ProductionSameItemSwitchStartContext::new(self, playlist_runtime, renderer);
            app_path.start(start, &mut lifecycle)
        };
        app_path.restore(&mut self.same_item_switch);
        if result.is_ok() {
            self.mark_pending_worker_redraw();
        }
        result
    }

    /// Продвигает shared strong envelope и публикует selectors только после exact Installed.
    pub(crate) fn poll_same_item_switch(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        if self.same_item_switch.is_none() {
            return;
        }
        let mut app_path = SameItemSwitchAppPath::take(&mut self.same_item_switch);
        {
            let mut context = ProductionSameItemSwitchPollContext::new(self, playlist_runtime);
            let _outcome = app_path.poll(&mut context);
        }
        app_path.restore(&mut self.same_item_switch);
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

fn picker_selection_intent(
    target: &crate::web_media_catalog::WebMediaSelectionTarget,
    stream_configuration: &crate::web_media_stream_model::WebMediaStreamConfiguration,
) -> Result<crate::web_media_open::YtDlpCandidateOpenIntent, SameItemSwitchError> {
    match target {
        #[cfg(test)]
        crate::web_media_catalog::WebMediaSelectionTarget::Fixture(_) => {
            Err(SameItemSwitchError::Stale)
        }
        crate::web_media_catalog::WebMediaSelectionTarget::Parent { selection } => Ok(
            crate::web_media_open::YtDlpCandidateOpenIntent::exact_parent_provider_default(
                selection.clone(),
                stream_configuration.preference(),
            ),
        ),
        crate::web_media_catalog::WebMediaSelectionTarget::Composed {
            selection,
            parent_preference,
        } => Ok(crate::web_media_open::YtDlpCandidateOpenIntent::composed(
            selection.clone(),
            parent_preference.clone(),
            stream_configuration.preference(),
        )),
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
        SameItemSwitchError::Stale | SameItemSwitchError::MissingActiveIdentity => {
            UrlSidebarSafeError::SameItemSwitchStale
        }
        SameItemSwitchError::ComponentAction(_) => UrlSidebarSafeError::SameItemSwitchStale,
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
