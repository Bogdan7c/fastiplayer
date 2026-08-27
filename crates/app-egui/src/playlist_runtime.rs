//! Process-lifetime shell playlist controller-а и reusable coordinators.
//!
//! Runtime владеет reusable media-open coordinator-ом, но по-прежнему не знает queue policy.
//! Он живёт в `AppShell`, а renderer-bound `AppState` получает короткоживущий binding
//! с новой generation и exact ordered player port после resume.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::app_wake::{AppWakePort, OwnerMailboxReceiver, owner_mailbox};
use crate::media_open::MediaOpenCoordinator;

#[allow(
    dead_code,
    reason = "Session 16 action API is rendered by Session 19 UI"
)]
mod actions;
mod compound_view;
#[allow(
    dead_code,
    reason = "Session 11A publishes controller foundation before Session 11B/12 UI callsites"
)]
mod controller;
mod desktop_transport;
pub(crate) mod discovery;
mod export_io;
mod external_projection;
#[allow(
    dead_code,
    reason = "Session 11A identities become production callsite inputs in subsequent sessions"
)]
mod identity;
mod import_io;
mod import_transaction;
mod lifecycle_checkpoint;
mod local_file_selection;
pub(crate) use lifecycle_checkpoint::LifecycleTimelineCheckpointPosition;
mod media_reset;
mod persistence;
mod persistence_runtime;
mod prepared_next;
#[allow(
    dead_code,
    reason = "Session 12A publishes runtime Undo boundary before Session 20 UI wiring"
)]
mod removal_undo;
#[allow(
    dead_code,
    reason = "Session 16 generalized model is rendered by Session 19 UI"
)]
mod replacement_confirmation;
mod resume_persistence;
pub(crate) use resume_persistence::InstalledCheckpointPosition;
mod row_interactions;
mod selection;
mod settings;
mod shell_boundary;
mod shutdown_report;
mod suspend_resume;
pub(crate) use suspend_resume::SuspendedTimelineResumePosition;
mod transport_execution;
pub(crate) use transport_execution::{PlaylistMediaOpenIntent, RelativeBeyondEndNavigationOutcome};
#[cfg(test)]
mod transport_execution_audit_regressions;
mod transport_ui;
mod ui_interaction;
mod url_import;
pub(crate) use import_transaction::{
    PlaylistImportContinueOutcome, PlaylistImportIntent, PlaylistImportIssueKind,
    PlaylistImportPreview, PlaylistImportPreviewId, PlaylistImportRejectedCount,
};
#[cfg(test)]
pub(crate) use import_transaction::{
    PlaylistImportPreviewUiAcceptedFixture, PlaylistImportPreviewUiCapacityFixture,
    PlaylistImportPreviewUiFixture,
};
pub(crate) use settings::{FutureDiscoveryPolicy, PlaylistSettingsStageError};
use shell_boundary::PlaylistRuntimeLifecycle;
pub(crate) use shell_boundary::{
    PlaylistAppStateAttachment, PlaylistBindingGeneration, PlaylistLifecycleGeneration,
    PlaylistOwnerCompletion, PlaylistOwnerPorts, PlaylistOwnerProgress, PlaylistRuntimeBinding,
    PlaylistTerminalShutdownOutcome,
};
#[allow(unused_imports)]
pub(crate) use shell_boundary::{PlaylistBindingRejection, PlaylistShutdownReport};
pub(crate) use suspend_resume::{
    ResumeAttempt, ResumeCheckpointError, ResumePlaybackIntent, ResumePositionWarning,
};
pub(crate) use transport_ui::{
    NavigationControlAvailability, PlaylistTransportUiModel, PlaylistUndoUiSnapshot,
    RemovalUndoUiModel,
};
#[cfg(test)]
pub(crate) use ui_interaction::{PlaylistActiveOperation, PlaylistSafeFeedback};
pub(crate) use ui_interaction::{
    PlaylistGoCurrentTarget, PlaylistInteractionModel, PlaylistManualAddEventId,
    PlaylistManualAddWarning, PlaylistManualAddWarningKind, PlaylistSafeFeedbackGeneration,
};
#[allow(
    dead_code,
    reason = "Session 14 bootstrap/save-worker integration consumes this startup boundary"
)]
mod startup;
mod startup_import;
pub(crate) use startup_import::StartupPlaylistImportTerminal;
mod startup_retained;
mod startup_runtime;
#[allow(unused_imports)]
pub(crate) use startup::{
    PlaylistLineagePersistence, PlaylistQueueGeneration, PlaylistStartupPhase,
    PlaylistStartupStateStore, PlaylistStartupView, PlaylistStartupWarning, RestoreApplyGeneration,
    StartupDraftError, StartupOwnerError,
};
pub(crate) use startup_retained::RetainedStartupApplyOutcome;
#[allow(
    dead_code,
    reason = "Session 11A read-only snapshot is attached by later playlist UI integration"
)]
mod view;
mod view_model;
mod web_media_catalog;

#[allow(
    unused_imports,
    reason = "Session 19 consumes typed Session 16 actions"
)]
pub(crate) use actions::{
    InstalledMetadataCacheOutcome, PlaylistConfirmationApplyOutcome, UrlAppendActionOutcome,
    UrlAppendValidationError,
};
pub(crate) use compound_view::{
    CompoundCurrentItemScrollTarget, CompoundHeaderPlayAction, CompoundHeaderPlayTarget,
    CompoundPartPlayAction, CompoundPartPlayTarget, CompoundPartPosition, CompoundRuntimeRow,
    CompoundRuntimeRowId, CompoundRuntimeViewSnapshot, CompoundRuntimeVisibleRow,
    PlaylistLayoutIdentity, ToggleCompoundDisclosure, ToggleCompoundDisclosureOutcome,
};
#[cfg(test)]
pub(crate) use controller::ControllerRemovalUndoOutcome;
pub(crate) use controller::PlaylistController;
pub(crate) use controller::SiblingDiscoveryScopeId;
pub(crate) use controller::{
    AutomaticLifecycleOutcome, ControllerInitialQueuePlaybackAction,
    ControllerManualNavigationOutcome, ControllerMoveItemsOutcome, ControllerPlayItemOutcome,
    ControllerStableIntentDispatch, LocalFileSelectionDisposition, PlannedPlaylistInstall,
    QueuePreloadTarget, StablePlaybackIntent, UnstagedPlannedTargetFailureOutcome,
};
pub(crate) use controller::{StartupPosition, StartupRestoreFailureOutcome, StartupRestoreTarget};
pub(crate) use discovery::PlaylistDiscoveryNavigationAction;
pub(crate) use export_io::{PlaylistExportRequest, PlaylistExportScopeIntent};
pub(crate) use identity::{ActiveMediaIdentity, TransportActionOrigin};
pub(crate) use media_reset::PlaylistMediaResetReceiptDisposition;
#[allow(
    unused_imports,
    reason = "typed persistence read model is consumed by upcoming playlist UI wiring"
)]
pub(crate) use persistence::{
    PlaylistPersistenceFault, PlaylistPersistenceView, PlaylistSaveDurability,
};
#[allow(unused_imports)]
pub(crate) use removal_undo::{RemovalUndoOutcome, RemovalUndoStatus, RuntimeRemovalOutcome};
#[allow(
    unused_imports,
    reason = "Session 19 consumes generalized confirmation model"
)]
pub(crate) use replacement_confirmation::{
    AdmittedLocalFileOpen, AdmittedQueueReplacementIntent, InAppQueueReplacementAdmission,
    InAppQueueReplacementIntent, PendingPlaylistConfirmation, PendingQueueReplacementConfirmation,
    PendingSensitiveUrlPersistenceDecision, PlaylistConfirmationAction,
    PlaylistConfirmationReasons, QueueReplacementConfirmationAction,
    QueueReplacementConfirmationDecision, QueueReplacementConfirmationOutcome,
    TrustedStartupQueueReplacementIntent, safe_local_open_label,
};
pub(crate) use row_interactions::{
    RuntimeCompoundHeaderPlayOutcome, RuntimeCompoundPartPlayOutcome, RuntimeMoveItemsOutcome,
    RuntimeRowPlayOutcome, RuntimeToggleCompoundDisclosureOutcome, RuntimeUpdateSelectionOutcome,
};
pub(crate) use selection::{
    ClearSelectionCursor, PlaylistSelectionSnapshot, UpdateSelection, UpdateSelectionOutcome,
};
#[cfg(test)]
pub(crate) use view::PlaylistVisibleRowTestFixture;
pub(crate) use view::{
    PlaylistStructuralActionAvailability, PlaylistStructuralRevision, PlaylistViewSnapshot,
    PlaylistVisibleRow,
};
#[cfg(test)]
pub(crate) use view_model::PlaylistSaveAttempt;
pub(crate) use view_model::{
    PlaylistNavigationView, PlaylistProbeView, PlaylistSaveView, PlaylistStartupWarningView,
    PlaylistViewModel,
};

/// D66 generation меняется только при queue-identity replacement boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManualAddQueueGeneration(u64);

impl ManualAddQueueGeneration {
    const INITIAL: Self = Self(1);

    const fn value(self) -> u64 {
        self.0
    }

    fn advance(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

/// Read-only load gate, который Session 11A свяжет с allocator/state startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Ready` становится достижимым после state-load wiring Session 14.
pub(crate) enum PlaylistLoadGateState {
    /// Trusted state/load decision ещё не получен.
    PendingLoadDecision,
    /// Trusted load decision разрешил domain commits указанной lineage policy.
    Open(PlaylistLineagePersistence),
}

/// Неблокирующий drain сообщает только policy transition, не filesystem details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "Session 14 shell drain wiring is landing in parallel"
)]
pub(crate) enum PlaylistStartupDrainOutcome {
    NoCompletion,
    ApplyingQuarantine,
    Ready,
    StaleCompletionIgnored,
}

/// Startup apply failures не превращаются в частично доступный controller.
#[derive(Debug, thiserror::Error)]
#[allow(
    dead_code,
    reason = "Session 14 shell drain wiring is landing in parallel"
)]
pub(crate) enum PlaylistStartupApplyError {
    #[error("playlist startup owner failed: {0:?}")]
    Owner(StartupOwnerError),
    #[error("validated allocator watermark could not initialize an empty queue")]
    AllocatorInvariant,
    #[error("playlist controller startup initialization failed: {0:?}")]
    Controller(controller::StartupControllerBuildError),
    #[error("prepared startup Add failed at the controller boundary: {0:?}")]
    Append(controller::ControllerAppendError),
}

/// Pre-gate draft admission сохраняет lifecycle и bounded-state ошибки раздельно.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupDraftAdmissionError {
    #[error("playlist startup draft is unavailable in the current phase: {0:?}")]
    Owner(StartupOwnerError),
    #[error("playlist startup draft rejected the mutation: {0:?}")]
    Draft(StartupDraftError),
}

/// Player staging/authorization закрыты отдельным D81 allocator gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistMediaOpenGateError {
    LoadDecisionPending,
    Coordinator(crate::media_open::MediaOpenCommandError),
    InstallAdmission(controller::PlaylistInstallAdmissionError),
    InstallReservation(playlist_core::PrepareReservedMutationError),
    ControllerInvariant(controller::PlaylistControllerInvariantViolation),
    InvalidPlaybackSpan,
    StalePlannedTarget,
}

/// Typed controller slot физически не содержит allocator до load decision.
struct PlaylistControllerSlot(Option<PlaylistController>);

impl PlaylistControllerSlot {
    const fn pending() -> Self {
        Self(None)
    }

    fn as_ref(&self) -> Option<&PlaylistController> {
        self.0.as_ref()
    }

    fn as_mut(&mut self) -> Option<&mut PlaylistController> {
        self.0.as_mut()
    }

    fn install(&mut self, controller: PlaylistController) {
        self.0 = Some(controller);
    }
}

#[cfg(test)]
impl std::ops::Deref for PlaylistControllerSlot {
    type Target = PlaylistController;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
            .expect("focused controller test must resolve startup load gate first")
    }
}

#[cfg(test)]
impl std::ops::DerefMut for PlaylistControllerSlot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
            .expect("focused controller test must resolve startup load gate first")
    }
}

/// Process-lifetime owner canonical controller-а и policy-neutral mechanism-ов.
pub(crate) struct PlaylistRuntime {
    lifecycle_generation: PlaylistLifecycleGeneration,
    next_binding_generation: PlaylistBindingGeneration,
    lifecycle: PlaylistRuntimeLifecycle,
    load_gate: PlaylistLoadGateState,
    /// Async inspection policy и единственный bounded pre-gate draft.
    startup: startup::PlaylistStartupOwner,
    /// Concrete state store, save worker и persistence read model живут process lifetime.
    persistence: persistence::PlaylistPersistenceOwner,
    resume_persistence: resume_persistence::PlaylistResumePersistenceOwner,
    admission_open: Arc<AtomicBool>,
    #[allow(dead_code)] // Поле удерживает worker-side ports process lifetime.
    owner_ports: PlaylistOwnerPorts,
    owner_receiver: OwnerMailboxReceiver<PlaylistOwnerProgress, PlaylistOwnerCompletion>,
    /// Canonical queue, identities и D08 guard живут независимо от renderer recreation.
    #[allow(
        dead_code,
        reason = "Session 11A foundation is attached by later UI orchestration"
    )]
    controller: PlaylistControllerSlot,
    /// Ровно один process-lifetime last-action removal Undo slot.
    removal_undo: Option<removal_undo::RemovalUndoState>,
    /// Последний exact Clear reset, ещё не принятый bounded player channel-ом.
    media_reset: media_reset::PlaylistMediaResetOwner,
    /// D79 confirmation хранит secret-bearing intent вне renderer-bound `AppState`.
    replacement_confirmation: replacement_confirmation::QueueReplacementConfirmationState,
    /// S08 latest-only import preview и staged commit принадлежат process runtime.
    import_transaction: import_transaction::PlaylistImportTransactionState,
    /// S09 single-root picker и bounded parser job живут отдельно от UI renderer-а.
    import_io: import_io::PlaylistImportIoOwner,
    /// S17S связывает trusted startup parse/preview/commit с exact first-item open receipt.
    startup_import: startup_import::StartupPlaylistImportState,
    /// S17 latest-only yt-dlp topology worker живёт process lifetime.
    url_import: url_import::PlaylistUrlImportOwner,
    /// S11 save dialog, pure preflight и atomic writer принадлежат process runtime.
    export_io: export_io::PlaylistExportIoOwner,
    /// CUE scope scan кешируется по immutable view revision, а не повторяется каждый frame.
    cue_export_availability_cache: RefCell<Option<export_io::CueExportAvailabilityCache>>,
    /// D48 form и async multi-file dialog принадлежат process runtime.
    ui_interaction: ui_interaction::PlaylistUiInteractionOwner,
    /// D66 stale guard для uncommitted Manual Add completions.
    manual_add_queue_generation: ManualAddQueueGeneration,
    /// D65 structural user intent invalidates late restore/CLI apply, not read-only load.
    startup_media_apply_superseded: bool,
    /// Bounded post-gate intent slot переживает cancel/enqueue race old startup install-а.
    startup_retained_actions: startup_retained::StartupRetainedActionOwner,
    /// Target-first sibling scope/executor переживает player advance и AppState recreation.
    discovery: discovery::PlaylistDiscoveryCoordinator,
    /// Process-lifetime reusable preparation/install mechanism Session 10C.
    media_open: MediaOpenCoordinator,
    /// Bounded speculative source/demux preparation не занимает auth reservation.
    prepared_next: prepared_next::PreparedNextOwner,
    /// Declared yt-dlp catalog и session-only semantic preference живут process lifetime.
    web_media_catalog: web_media_catalog::PlaylistWebMediaCatalogOwner,
    /// Runtime-only active source/checkpoint переживают renderer-bound `AppState` recreation.
    suspended_media: suspend_resume::SuspendedMediaState,
    settings: settings::PlaylistSettingsOwner,
    /// MPRIS backend/mailbox/snapshot/volume переживают renderer-bound AppState.
    desktop_transport: Option<desktop_transport::DesktopTransportOwner>,
}

impl PlaylistRuntime {
    /// Вводит уже подготовленное source-owner-ом media без повторного demux open.
    pub(crate) fn start_prepared_media_open(
        &mut self,
        client_key: crate::media_open::MediaOpenClientKey,
        prepared_open: crate::media_open::PreparedMediaOpen,
        safe_label: crate::media_open::SafeMediaLabel,
    ) -> Result<crate::media_open::MediaOpenStartOutcome, crate::media_open::MediaOpenStartError>
    {
        self.prepared_next
            .cancel(player_core::MediaInstallCancellationCause::Superseded);
        self.media_open
            .start_prepared(client_key, prepared_open, safe_label)
    }

    /// Создаёт runtime один раз вместе с `AppShell`, до любого `AppState`.
    #[cfg(test)]
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self::new_with_config(wake_port, rustiplayer_config::PlaylistConfig::default())
    }

    /// Создаёт runtime с policy из уже валидированного startup config.
    #[cfg(test)]
    pub(crate) fn new_with_config(
        wake_port: AppWakePort,
        playlist_config: rustiplayer_config::PlaylistConfig,
    ) -> Self {
        Self::new_with_resume_policy(wake_port, playlist_config, true)
    }

    /// Production constructor дополнительно принимает существующую player enable-policy.
    pub(crate) fn new_with_resume_policy(
        wake_port: AppWakePort,
        playlist_config: rustiplayer_config::PlaylistConfig,
        resume_last_position: bool,
    ) -> Self {
        let ui_interaction = ui_interaction::PlaylistUiInteractionOwner::new(wake_port.clone());
        let import_io = import_io::PlaylistImportIoOwner::new(wake_port.clone());
        let url_import = url_import::PlaylistUrlImportOwner::new(wake_port.clone());
        let export_io = export_io::PlaylistExportIoOwner::new(wake_port.clone());
        let desktop_transport = desktop_transport::DesktopTransportOwner::new(wake_port.clone());
        let media_open = MediaOpenCoordinator::new(wake_port.clone());
        let prepared_next =
            prepared_next::PreparedNextOwner::new(wake_port.clone(), playlist_config);
        let web_media_catalog = web_media_catalog::PlaylistWebMediaCatalogOwner::new();
        let startup = startup::PlaylistStartupOwner::new(wake_port.clone());
        let discovery = discovery::PlaylistDiscoveryCoordinator::new(wake_port.clone());
        let (publisher, owner_receiver) = owner_mailbox(wake_port);
        let admission_open = Arc::new(AtomicBool::new(true));
        let persistence =
            persistence::PlaylistPersistenceOwner::new(playlist_config.state_save_debounce_ms);
        let resume_persistence = resume_persistence::PlaylistResumePersistenceOwner::new(
            playlist_config.resume_checkpoint_interval_ms,
            resume_last_position,
        );
        let mut settings = settings::PlaylistSettingsOwner::new(playlist_config);
        settings.install_save_debounce_port(persistence.debounce_port());
        settings.install_resume_interval_port(resume_persistence.interval_port());
        settings.install_discovery_port(discovery.settings_port());
        Self {
            lifecycle_generation: PlaylistLifecycleGeneration(0),
            next_binding_generation: PlaylistBindingGeneration(0),
            lifecycle: PlaylistRuntimeLifecycle::Suspended,
            load_gate: PlaylistLoadGateState::PendingLoadDecision,
            startup,
            persistence,
            resume_persistence,
            owner_ports: PlaylistOwnerPorts {
                publisher,
                admission_open: admission_open.clone(),
            },
            admission_open,
            owner_receiver,
            controller: PlaylistControllerSlot::pending(),
            removal_undo: None,
            media_reset: media_reset::PlaylistMediaResetOwner::default(),
            replacement_confirmation:
                replacement_confirmation::QueueReplacementConfirmationState::new(),
            import_transaction: import_transaction::PlaylistImportTransactionState::new(),
            import_io,
            startup_import: startup_import::StartupPlaylistImportState::default(),
            url_import,
            export_io,
            cue_export_availability_cache: RefCell::new(None),
            ui_interaction,
            manual_add_queue_generation: ManualAddQueueGeneration::INITIAL,
            startup_media_apply_superseded: false,
            startup_retained_actions: startup_retained::StartupRetainedActionOwner::default(),
            discovery,
            media_open,
            prepared_next,
            web_media_catalog,
            suspended_media: suspend_resume::SuspendedMediaState::default(),
            settings,
            desktop_transport: Some(desktop_transport),
        }
    }
}

impl PlaylistRuntime {
    pub(crate) fn preflight_playlist_settings(&self) -> Result<(), String> {
        self.settings.preflight()
    }

    pub(crate) fn stage_playlist_settings(
        &mut self,
        requested: rustiplayer_config::PlaylistConfig,
    ) -> Result<bool, settings::PlaylistSettingsStageError> {
        let controller = self.controller.as_mut().ok_or_else(|| {
            settings::PlaylistSettingsStageError::Failed(
                "playlist allocator load decision is still pending".to_owned(),
            )
        })?;
        let changed = self.settings.stage(requested, controller)?;
        let committed = self.settings.committed();
        self.prepared_next.reconfigure(committed);
        if !committed.next_item_preload_enabled {
            controller.cancel_next_item_preload_plan();
        }
        Ok(changed)
    }

    #[allow(dead_code)] // Session 14 подключит следующий explicit-open discovery job к snapshot boundary.
    pub(crate) fn future_playlist_discovery_policy(&self) -> FutureDiscoveryPolicy {
        self.settings.future_discovery_policy()
    }

    #[allow(dead_code)] // Подключается transport callsite-ами вместе с playlist UI в следующей wiring session.
    pub(crate) fn previous_restart_threshold(&self) -> controller::PreviousRestartThreshold {
        self.settings.previous_restart_threshold()
    }

    pub(crate) fn rollback_playlist_settings(&mut self) -> Result<bool, String> {
        let controller = self
            .controller
            .as_mut()
            .ok_or_else(|| "playlist allocator load decision is still pending".to_owned())?;
        let changed = self.settings.rollback(controller)?;
        let committed = self.settings.committed();
        self.prepared_next.reconfigure(committed);
        if !committed.next_item_preload_enabled {
            controller.cancel_next_item_preload_plan();
        }
        Ok(changed)
    }

    pub(crate) fn finalize_playlist_settings(&mut self) {
        self.settings.finalize();
    }
}

#[allow(
    dead_code,
    reason = "Session 10C publishes intent API before callsite migration in Session 10D"
)]
impl PlaylistRuntime {
    /// Запускает source preparation по explicit caller command без queue policy внутри runtime.
    pub(crate) fn start_media_open(
        &mut self,
        client_key: crate::media_open::MediaOpenClientKey,
        source_request: crate::media_open::MediaOpenSourceRequest,
        mode: crate::media_open::MediaOpenStartMode,
    ) -> Result<crate::media_open::MediaOpenStartOutcome, crate::media_open::MediaOpenStartError>
    {
        self.prepared_next
            .cancel(player_core::MediaInstallCancellationCause::Superseded);
        self.media_open.start(client_key, source_request, mode)
    }

    /// Supersede-ит только exact pre-player request по решению caller-а.
    pub(crate) fn supersede_media_open_before_player_staging(
        &mut self,
        expected_request_id: crate::media_open::MediaOpenRequestId,
        client_key: crate::media_open::MediaOpenClientKey,
        source_request: crate::media_open::MediaOpenSourceRequest,
    ) -> Result<crate::media_open::MediaOpenStartOutcome, crate::media_open::MediaOpenStartError>
    {
        self.media_open.supersede_prepared_or_preparing(
            expected_request_id,
            client_key,
            source_request,
        )
    }

    /// Передаёт prepared media exact player request-у после app-owned resource staging.
    pub(crate) fn stage_media_open_at_player(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        intent: crate::media_open::MediaOpenInstallIntent,
        video_resource_port: player_core::MediaInstallVideoResourcePort,
    ) -> Result<player_core::MediaInstallRequestId, PlaylistMediaOpenGateError> {
        if !matches!(self.load_gate, PlaylistLoadGateState::Open(_)) {
            return Err(PlaylistMediaOpenGateError::LoadDecisionPending);
        }
        self.media_open
            .stage_at_player(request_id, intent, video_resource_port)
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    /// Немедленно dispatch-ит matching authorization без второго buffer-а.
    pub(crate) fn authorize_ready_media_open(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<crate::media_open::AuthorizationDispatchResolution, PlaylistMediaOpenGateError>
    {
        if !matches!(self.load_gate, PlaylistLoadGateState::Open(_)) {
            return Err(PlaylistMediaOpenGateError::LoadDecisionPending);
        }
        self.media_open
            .authorize_ready(request_id)
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    /// Отправляет exact typed cancel либо сообщает, что enqueue barrier уже выиграл.
    pub(crate) fn cancel_media_open(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
    ) -> Result<
        crate::media_open::CancellationDispatchOutcome,
        crate::media_open::MediaOpenCommandError,
    > {
        self.media_open.cancel_request(request_id, cause)
    }

    /// Cleanup после pre-barrier dispatch rejection не теряется на повторном backpressure.
    pub(crate) fn cancel_media_open_lossless(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
    ) -> Result<
        crate::media_open::CancellationDispatchOutcome,
        crate::media_open::MediaOpenCommandError,
    > {
        self.media_open.cancel_request_lossless(request_id, cause)
    }

    /// Forward-ит D52 update только в matching player request/instance boundary.
    pub(crate) fn update_media_open_playback_intent(
        &self,
        request_id: crate::media_open::MediaOpenRequestId,
        revision: player_core::PlaybackIntentRevision,
        intent: player_core::PlaybackIntent,
    ) -> Result<player_core::PlaybackIntentUpdateReceipt, crate::media_open::MediaOpenCommandError>
    {
        self.media_open
            .update_playback_intent(request_id, revision, intent)
    }

    /// Возвращает read-only snapshot typed phase для caller orchestration.
    pub(crate) fn media_open_snapshot(&self) -> Option<crate::media_open::MediaOpenSnapshot> {
        self.media_open.snapshot()
    }

    /// Синхронно ждёт только exact request-owned protocol progress без timeout-as-success.
    pub(crate) fn wait_for_media_open_progress(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<crate::media_open::MediaOpenPhase, crate::media_open::MediaOpenCompletionDriveError>
    {
        self.media_open.wait_for_progress(request_id)
    }

    /// Забирает request-owned terminal exactly once после полного caller commit/abort flow.
    pub(crate) fn take_media_open_terminal(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<
        Option<crate::media_open::MediaOpenTerminalOutcome>,
        crate::media_open::MediaOpenCommandError,
    > {
        self.media_open.take_terminal(request_id)
    }
}

#[cfg(test)]
mod tests;
