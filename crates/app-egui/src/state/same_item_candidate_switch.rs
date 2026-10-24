//! S25/S36 controlled same-item candidate/component switch orchestration.

mod audio_language;
mod lifecycle_bridge;

use player_core::MediaInstallCancellationCause;
use render_shell_api::RenderShell;

use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution,
};
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarPendingSelection, UrlSidebarSafeError, WebMediaStreamGeneration,
};

use super::{ActiveMediaSource, AppState, StrongMediaOpenError, StrongMediaOpenPoll};
pub(super) use audio_language::AudioLanguagePurpose;
use audio_language::SameItemSelectionRequest;
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

/// Откуда пришёл component switch: одна транзакция, но разные адресаты сообщения об отказе.
///
/// Sidebar показывает отказ inline (`UrlSidebarSafeError`) в любом случае; меню «♪» ещё и
/// получает toast, потому что пользователь мог вообще не открывать sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComponentSwitchOrigin {
    /// Строка component-варианта в URL sidebar.
    UrlSidebar,
    /// Web-версия звука в меню аудиодорожек или клавиша `B`.
    AudioTrackMenu,
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
    Component {
        action: crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction,
        /// Кто запустил switch: от этого зависит, куда сообщить о терминальном отказе.
        origin: ComponentSwitchOrigin,
    },
    /// Другой язык звука (дубляж) того же ролика; видео и позиция не меняются.
    AudioLanguage {
        parent_generation: WebMediaStreamGeneration,
        /// Выбор пользователя или восстановление из памяти.
        purpose: AudioLanguagePurpose,
    },
    Picker {
        parent_generation: WebMediaStreamGeneration,
        action: crate::web_media_catalog::WebMediaFacetAction,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    },
    AutomaticPicker {
        parent_generation: WebMediaStreamGeneration,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    },
    /// Automatic runtime quality step не сохраняется как manual item preference.
    AdaptiveQuality {
        parent_generation: WebMediaStreamGeneration,
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
            Self::Component { action, .. } => UrlSidebarPendingSelection::Component(*action),
            Self::Picker {
                parent_generation,
                action,
                ..
            } => UrlSidebarPendingSelection::StreamFacet {
                parent_generation: *parent_generation,
                action: *action,
            },
            // Языка нет среди sidebar-строк: пока идёт switch, sidebar просто блокируется.
            Self::AudioLanguage {
                parent_generation, ..
            }
            | Self::AutomaticPicker {
                parent_generation, ..
            }
            | Self::AdaptiveQuality {
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
            Self::Component { action, .. } => action.parent_generation(),
            Self::AudioLanguage {
                parent_generation, ..
            }
            | Self::Picker {
                parent_generation, ..
            }
            | Self::AutomaticPicker {
                parent_generation, ..
            }
            | Self::AdaptiveQuality {
                parent_generation, ..
            } => *parent_generation,
        }
    }
}

impl PendingSameItemSwitch {
    /// Источник component switch-а; `None` — это не component switch (candidate/picker/качество).
    fn component_origin(&self) -> Option<ComponentSwitchOrigin> {
        match self.kind {
            SameItemSwitchKind::Component { origin, .. } => Some(origin),
            // Выбор пользователя: отказ должен дойти до toast-а меню «♪». Автоматическое
            // восстановление из памяти молчит.
            SameItemSwitchKind::AudioLanguage {
                purpose: AudioLanguagePurpose::UserChoice { .. },
                ..
            } => Some(ComponentSwitchOrigin::AudioTrackMenu),
            SameItemSwitchKind::AudioLanguage {
                purpose: AudioLanguagePurpose::MemoryRestore,
                ..
            } => None,
            #[cfg(test)]
            SameItemSwitchKind::Candidate { .. } => None,
            SameItemSwitchKind::Picker { .. }
            | SameItemSwitchKind::AutomaticPicker { .. }
            | SameItemSwitchKind::AdaptiveQuality { .. } => None,
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
    #[error("The same-item media switch is already running.")]
    Busy,
    /// UI intent больше не соответствует installed source generation.
    #[error("same-item media switch intent is outdated")]
    Stale,
    /// Component action не прошёл generation/axis/index validation владельца catalog-а.
    #[error(transparent)]
    ComponentAction(#[from] ComponentVariantActionError),
    /// Active source не публикует переключаемый neutral catalog contract.
    #[error("active source supportive same-item media switch")]
    UnsupportedSource,
    /// App/controller больше не подтверждают exact active media identity.
    #[error("active media identity missing or already modified")]
    MissingActiveIdentity,
    /// Renderer-bound playlist binding неактивен.
    #[error("playlist runtime binding unavailable")]
    MissingBinding,
    /// Capability snapshot ещё не опубликован composition owner-ом.
    #[error("system capability snapshot unavailable")]
    MissingCapabilities,
    /// Player lifecycle preflight не удалось прочитать без потери причины.
    #[error("player runtime preflight flattened same-item media switch: {0}")]
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
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        self.apply_same_item_action(
            action,
            ComponentSwitchOrigin::UrlSidebar,
            playlist_runtime,
            renderer,
        )
    }

    /// Запускает выбор web-версии звука из меню «♪» тем же путём, что строка URL sidebar.
    ///
    /// Отличие одно: origin `AudioTrackMenu`, чтобы терминальный отказ дошёл и до toast-а.
    pub(super) fn apply_audio_track_menu_component_action(
        &mut self,
        action: crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        self.apply_same_item_action(
            UrlSidebarAction::ComponentVariant(action),
            ComponentSwitchOrigin::AudioTrackMenu,
            playlist_runtime,
            renderer,
        )
    }

    /// Общий запуск: start failure публикуется в sidebar как generation-scoped safe error.
    fn apply_same_item_action(
        &mut self,
        action: UrlSidebarAction,
        origin: ComponentSwitchOrigin,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let generation = action.parent_generation();
        let result = self.start_same_item_switch(action, origin, playlist_runtime, renderer);
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
        origin: ComponentSwitchOrigin,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let active_source = self.preflight_same_item_switch()?;
        let stream_configuration = active_source
            .web_intent()
            .and_then(crate::media_open::WebMediaSourceIntent::stream_configuration)
            .ok_or(SameItemSwitchError::UnsupportedSource)?;
        let (selection_intent, kind) = match action {
            #[cfg(test)]
            UrlSidebarAction::Candidate {
                generation,
                candidate_index,
            } => {
                let selection = stream_configuration
                    .selection_for_switch(generation, candidate_index)
                    .ok_or(SameItemSwitchError::Stale)?;
                let candidate_presentation = stream_configuration
                    .candidates()
                    .get(candidate_index)
                    .cloned()
                    .ok_or(SameItemSwitchError::Stale)?;
                if candidate_presentation == *stream_configuration.active_candidate() {
                    return Ok(UrlSidebarActionApplyOutcome::NoChange);
                }
                let preferred_height = candidate_presentation.height;
                (
                    crate::media_open::WebMediaSelectionSwitchIntent::CatalogTarget(
                        crate::web_media_catalog::WebMediaSelectionTarget::Candidate {
                            selection: Box::new(selection),
                        },
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
                (
                    crate::media_open::WebMediaSelectionSwitchIntent::ComponentSemantic(
                        semantic_selection,
                    ),
                    SameItemSwitchKind::Component {
                        action: component_action,
                        origin,
                    },
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
                if catalog.parent_generation() != Some(parent_generation) {
                    return Err(SameItemSwitchError::Stale);
                }
                let target = catalog
                    .resolve_facet_action(action)
                    .cloned()
                    .ok_or(SameItemSwitchError::Stale)?;
                if target == catalog.active_choice().target {
                    return Ok(UrlSidebarActionApplyOutcome::NoChange);
                }
                (
                    crate::media_open::WebMediaSelectionSwitchIntent::CatalogTarget(target.clone()),
                    SameItemSwitchKind::Picker {
                        parent_generation,
                        action,
                        target,
                    },
                )
            }
        };
        self.start_resolved_same_item_switch(
            active_source,
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
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let active_source = self.preflight_same_item_switch()?;
        let stream_configuration = active_source
            .web_intent()
            .and_then(crate::media_open::WebMediaSourceIntent::stream_configuration)
            .ok_or(SameItemSwitchError::UnsupportedSource)?;
        if stream_configuration.generation() != pending.parent_generation {
            return Err(SameItemSwitchError::Stale);
        }
        let crate::web_media_catalog::WebMediaCatalogState::Ready(catalog) =
            &self.web_media_catalog_state
        else {
            return Err(SameItemSwitchError::Stale);
        };
        if catalog.generation() != pending.catalog_generation
            || catalog.parent_generation() != Some(pending.parent_generation)
            || !catalog.contains_target(&pending.target)
        {
            return Err(SameItemSwitchError::Stale);
        }
        if pending.target == catalog.active_choice().target {
            return Ok(UrlSidebarActionApplyOutcome::NoChange);
        }
        let switch_kind = match pending.purpose {
            super::web_media_catalog::AutomaticWebMediaSwitchPurpose::RememberedPreference => {
                SameItemSwitchKind::AutomaticPicker {
                    parent_generation: pending.parent_generation,
                    target: pending.target.clone(),
                }
            }
            super::web_media_catalog::AutomaticWebMediaSwitchPurpose::AdaptiveQuality => {
                SameItemSwitchKind::AdaptiveQuality {
                    parent_generation: pending.parent_generation,
                }
            }
        };
        self.start_resolved_same_item_switch(
            active_source,
            crate::media_open::WebMediaSelectionSwitchIntent::CatalogTarget(pending.target.clone()),
            switch_kind,
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
        selection_intent: impl Into<SameItemSelectionRequest>,
        kind: SameItemSwitchKind,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &mut dyn RenderShell,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        let selection_intent = selection_intent.into();
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
        let settings = crate::media_open::WebMediaOpenSettings::from_app_config(
            &config,
            &capabilities,
            self.audio_decode_capability_snapshot(),
        );
        #[expect(
            clippy::expect_used,
            reason = "инвариант: same-item web source был проверен до lifecycle start"
        )]
        let physical_request = match selection_intent.resolve(
            active_source
                .web_intent()
                .expect("The same-item web source was checked before lifecycle start."),
            settings,
        ) {
            crate::media_open::WebMediaSelectionSwitchResolution::NoChange => {
                return Ok(UrlSidebarActionApplyOutcome::NoChange);
            }
            crate::media_open::WebMediaSelectionSwitchResolution::Ready(request) => {
                MediaOpenSourceRequest::Web(request)
            }
            crate::media_open::WebMediaSelectionSwitchResolution::Unsupported => {
                return Err(SameItemSwitchError::UnsupportedSource);
            }
            crate::media_open::WebMediaSelectionSwitchResolution::Stale => {
                return Err(SameItemSwitchError::Stale);
            }
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
    pub(crate) fn poll_same_item_switch(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        video_frames: &mut dyn render_shell_api::VideoFrameImport,
    ) {
        if self.same_item_switch.is_none() {
            return;
        }
        // Origin читается до poll-а: terminal poll забирает pending вместе с kind.
        let component_origin = self
            .same_item_switch
            .as_ref()
            .and_then(PendingSameItemSwitch::component_origin);
        let mut app_path = SameItemSwitchAppPath::take(&mut self.same_item_switch);
        let outcome = {
            let mut context =
                ProductionSameItemSwitchPollContext::new(self, playlist_runtime, video_frames);
            app_path.poll(&mut context)
        };
        app_path.restore(&mut self.same_item_switch);
        // Отказ switch-а, запущенного из меню «♪», дополнительно сообщается toast-ом.
        if component_origin == Some(ComponentSwitchOrigin::AudioTrackMenu)
            && let lifecycle_bridge::SameItemSwitchAppPoll::Failed(error) = outcome
        {
            self.report_web_audio_version_switch_failed(error);
        }
        self.mark_pending_worker_redraw();
    }

    /// Suspend отменяет pre-barrier работу либо дренирует commit-winner до rebind-а.
    ///
    /// Рендер нужен, если commit-winner успеет дойти до Installed: импорт кадров кандидата
    /// ставится в него. Переключение без рендера — нарушение инварианта shell-а.
    pub(crate) fn resolve_same_item_switch_for_suspend(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: Option<&mut Box<dyn render_shell_api::RenderShell>>,
    ) -> Result<(), crate::playlist_runtime::ResumeCheckpointError> {
        let Some(pending) = self.same_item_switch.as_ref() else {
            return Ok(());
        };
        let Some(renderer) = renderer else {
            return Err(crate::playlist_runtime::ResumeCheckpointError::ControllerInvariant);
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
            self.poll_same_item_switch(playlist_runtime, renderer.video_frames());
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
    /// Возвращает freshly Installed provider-neutral configuration.
    pub(super) fn web_media_stream_configuration(
        &self,
    ) -> Option<&crate::web_media_stream_model::WebMediaStreamConfiguration> {
        self.web_intent()
            .and_then(crate::media_open::WebMediaSourceIntent::stream_configuration)
    }

    /// Извлекает generation только из freshly Installed catalog-backed source.
    fn web_media_stream_generation(&self) -> Option<WebMediaStreamGeneration> {
        self.web_media_stream_configuration()
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
