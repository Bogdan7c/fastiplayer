//! Process-lifetime shell playlist controller-а и reusable coordinators.
//!
//! Runtime владеет reusable media-open coordinator-ом, но по-прежнему не знает queue policy.
//! Он живёт в `AppShell`, а renderer-bound `AppState` получает короткоживущий binding
//! с новой generation и exact ordered player port после resume.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use playlist_core::PlaylistQueue;

use crate::app_wake::{
    AppWakePort, OwnerMailboxPublisher, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::MediaOpenCoordinator;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

#[allow(
    dead_code,
    reason = "Session 16 action API is rendered by Session 19 UI"
)]
mod actions;
#[allow(
    dead_code,
    reason = "Session 11A publishes controller foundation before Session 11B/12 UI callsites"
)]
mod controller;
mod desktop_transport;
pub(crate) mod discovery;
#[allow(
    dead_code,
    reason = "Session 11A identities become production callsite inputs in subsequent sessions"
)]
mod identity;
mod local_file_selection;
mod persistence;
mod persistence_runtime;
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
mod row_interactions;
mod settings;
mod suspend_resume;
mod transport_execution;
mod transport_ui;
mod ui_interaction;
pub(crate) use settings::{FutureDiscoveryPolicy, PlaylistSettingsStageError};
pub(crate) use suspend_resume::{
    ResumeAttempt, ResumeCheckpointError, ResumePlaybackIntent, ResumePositionWarning,
};
pub(crate) use transport_ui::{NavigationControlAvailability, PlaylistTransportUiModel};
pub(crate) use ui_interaction::{
    PlaylistGoCurrentTarget, PlaylistInteractionModel, PlaylistProgressCancelScope,
    PlaylistProgressModel, PlaylistWaitDirection,
};
#[allow(
    dead_code,
    reason = "Session 14 bootstrap/save-worker integration consumes this startup boundary"
)]
mod startup;
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

#[allow(
    unused_imports,
    reason = "Session 19 consumes typed Session 16 actions"
)]
pub(crate) use actions::{
    InstalledMetadataCacheOutcome, PlaylistConfirmationApplyOutcome, UrlAppendActionOutcome,
    UrlAppendValidationError,
};
pub(crate) use controller::PlaylistController;
pub(crate) use controller::{
    AutomaticLifecycleOutcome, ControllerInitialQueuePlaybackAction,
    ControllerManualNavigationOutcome, ControllerMoveItemOutcome, ControllerPlayItemOutcome,
    ControllerStableIntentDispatch, LocalFileSelectionDisposition, PlannedPlaylistInstall,
    StablePlaybackIntent, StopAfterCurrentOutcome,
};
pub(crate) use controller::{StartupRestoreFailureOutcome, StartupRestoreTarget};
pub(crate) use discovery::{MetadataSortCancelOutcome, PlaylistDiscoveryNavigationAction};
pub(crate) use identity::TransportActionOrigin;
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
pub(crate) use row_interactions::{RuntimeMoveItemOutcome, RuntimeRowPlayOutcome};
#[cfg(test)]
pub(crate) use view::PlaylistVisibleRowTestFixture;
pub(crate) use view::{PlaylistStructuralRevision, PlaylistViewSnapshot, PlaylistVisibleRow};
pub(crate) use view_model::{
    PlaylistLoadingView, PlaylistNavigationView, PlaylistProbeView, PlaylistSaveView,
    PlaylistStartupWarningView, PlaylistViewModel,
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

/// Generation любого lifecycle transition runtime-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistLifecycleGeneration(u64);

/// Generation конкретного renderer/player binding-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistBindingGeneration(u64);

/// Exact binding token будущих AppState/controller callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistRuntimeBinding {
    lifecycle_generation: PlaylistLifecycleGeneration,
    binding_generation: PlaylistBindingGeneration,
}

/// Renderer-bound attachment: exact port identity плюс immutable controller view.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistAppStateAttachment {
    binding: PlaylistRuntimeBinding,
    view_model: PlaylistViewModel,
}

impl PlaylistAppStateAttachment {
    /// Возвращает exact binding для correlation callbacks текущего `AppState`.
    pub(crate) const fn binding(&self) -> PlaylistRuntimeBinding {
        self.binding
    }

    /// Возвращает cheap-clone read-only snapshot без mutable доступа к controller-у.
    pub(crate) fn view_model(&self) -> PlaylistViewModel {
        self.view_model.clone()
    }

    /// Заменяет только immutable view при сохранении exact binding identity.
    pub(crate) fn replace_view_model(&mut self, view_model: PlaylistViewModel) {
        self.view_model = view_model;
    }
}

impl PlaylistRuntimeBinding {
    /// Передаёт controller-у exact player binding generation после `Installed`.
    pub(crate) const fn binding_generation(self) -> PlaylistBindingGeneration {
        self.binding_generation
    }
}

/// Почему callback нельзя применять к текущему runtime binding-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Session 11A начнёт применять этот typed rejection на callbacks.
pub(crate) enum PlaylistBindingRejection {
    /// Runtime сейчас suspended и renderer/player binding отсутствует.
    Suspended,
    /// Callback относится к предыдущей lifecycle/binding generation.
    StaleGeneration,
    /// Runtime уже начал либо завершил process shutdown.
    ShuttingDown,
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
    StalePlannedTarget,
}

/// Текущая lifecycle фаза process owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistRuntimeLifecycle {
    Suspended,
    Bound(PlaylistRuntimeBinding),
    ShuttingDown,
    Shutdown,
}

/// Полный typed отчёт всех process-lifetime playlist owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistShutdownReport {
    pub(crate) ui_interaction: ProcessOwnerShutdownOutcome,
    pub(crate) media_open: ProcessOwnerShutdownOutcome,
    pub(crate) startup: startup::PlaylistStartupShutdownOutcome,
    pub(crate) persistence: persistence::PlaylistPersistenceShutdownOutcome,
}

/// Результат нового terminal API с общей абсолютной границей времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistTerminalShutdownOutcome {
    /// Все owners терминальны, filesystem outcome успешен.
    Completed(PlaylistShutdownReport),
    /// Runtime уже полностью завершён предыдущим вызовом.
    AlreadyCompleted,
    /// Хотя бы один writable/async поток не подтверждён; lease освобождать нельзя.
    ExitRequired(PlaylistShutdownReport),
    /// Все потоки терминальны, но panic либо persistence failure сохранены типизированно.
    Failed(PlaylistShutdownReport),
}

/// Пока controller отсутствует, progress/completion slots несут neutral marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistOwnerProgress;

/// Neutral terminal marker будущего playlist owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistOwnerCompletion;

/// Cloneable worker-side ports, принадлежащие process runtime-у.
#[derive(Clone)]
#[allow(dead_code)] // Ports резервируют process boundary до controller Session 11A.
pub(crate) struct PlaylistOwnerPorts {
    publisher: OwnerMailboxPublisher<PlaylistOwnerProgress, PlaylistOwnerCompletion>,
    admission_open: Arc<AtomicBool>,
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

impl PlaylistOwnerPorts {
    /// Публикует latest progress только пока shutdown gate допускает работу.
    fn publish_progress(&self) -> bool {
        if !self.admission_open.load(Ordering::Acquire) {
            return false;
        }
        !matches!(
            self.publisher
                .publish_progress(PlaylistOwnerProgress)
                .wake_delivery(),
            WakeDelivery::EventLoopClosed
        )
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
    /// D79 confirmation хранит secret-bearing intent вне renderer-bound `AppState`.
    replacement_confirmation: replacement_confirmation::QueueReplacementConfirmationState,
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
        self.media_open
            .start_prepared(client_key, prepared_open, safe_label)
    }

    /// Создаёт runtime один раз вместе с `AppShell`, до любого `AppState`.
    #[cfg(test)]
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self::new_with_config(wake_port, rustiplayer_config::PlaylistConfig::default())
    }

    /// Создаёт runtime с policy из уже валидированного startup config.
    pub(crate) fn new_with_config(
        wake_port: AppWakePort,
        playlist_config: rustiplayer_config::PlaylistConfig,
    ) -> Self {
        let ui_interaction = ui_interaction::PlaylistUiInteractionOwner::new(wake_port.clone());
        let desktop_transport = desktop_transport::DesktopTransportOwner::new(wake_port.clone());
        let media_open = MediaOpenCoordinator::new(wake_port.clone());
        let startup = startup::PlaylistStartupOwner::new(wake_port.clone());
        let discovery = discovery::PlaylistDiscoveryCoordinator::new(wake_port.clone());
        let (publisher, owner_receiver) = owner_mailbox(wake_port);
        let admission_open = Arc::new(AtomicBool::new(true));
        let persistence =
            persistence::PlaylistPersistenceOwner::new(playlist_config.state_save_debounce_ms);
        let mut settings = settings::PlaylistSettingsOwner::new(playlist_config);
        settings.install_save_debounce_port(persistence.debounce_port());
        settings.install_discovery_port(discovery.settings_port());
        Self {
            lifecycle_generation: PlaylistLifecycleGeneration(0),
            next_binding_generation: PlaylistBindingGeneration(0),
            lifecycle: PlaylistRuntimeLifecycle::Suspended,
            load_gate: PlaylistLoadGateState::PendingLoadDecision,
            startup,
            persistence,
            owner_ports: PlaylistOwnerPorts {
                publisher,
                admission_open: admission_open.clone(),
            },
            admission_open,
            owner_receiver,
            controller: PlaylistControllerSlot::pending(),
            removal_undo: None,
            replacement_confirmation:
                replacement_confirmation::QueueReplacementConfirmationState::new(),
            ui_interaction,
            manual_add_queue_generation: ManualAddQueueGeneration::INITIAL,
            startup_media_apply_superseded: false,
            startup_retained_actions: startup_retained::StartupRetainedActionOwner::default(),
            discovery,
            media_open,
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
        self.settings.stage(requested, controller)
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
        self.settings.rollback(controller)
    }

    pub(crate) fn finalize_playlist_settings(&mut self) {
        self.settings.finalize();
    }

    /// Создаёт новый exact binding после успешного AppState recreation.
    pub(crate) fn bind_resumed_app_state(&mut self) -> Option<PlaylistRuntimeBinding> {
        if matches!(
            self.lifecycle,
            PlaylistRuntimeLifecycle::ShuttingDown | PlaylistRuntimeLifecycle::Shutdown
        ) {
            return None;
        }

        self.lifecycle_generation.0 = self
            .lifecycle_generation
            .0
            .checked_add(1)
            .expect("playlist lifecycle generation overflow during bind");
        self.next_binding_generation.0 = self
            .next_binding_generation
            .0
            .checked_add(1)
            .expect("playlist binding generation overflow during bind");
        let binding = PlaylistRuntimeBinding {
            lifecycle_generation: self.lifecycle_generation,
            binding_generation: self.next_binding_generation,
        };
        self.lifecycle = PlaylistRuntimeLifecycle::Bound(binding);
        Some(binding)
    }

    /// Снимает только AppState binding; process owner, ports и load gate сохраняются.
    pub(crate) fn suspend_app_state_binding(&mut self) {
        if matches!(self.lifecycle, PlaylistRuntimeLifecycle::Bound(_)) {
            self.lifecycle_generation.0 = self
                .lifecycle_generation
                .0
                .checked_add(1)
                .expect("playlist lifecycle generation overflow during suspend");
            self.lifecycle = PlaylistRuntimeLifecycle::Suspended;
            self.media_open.suspend_player_binding();
        }
    }

    /// Привязывает exact ordered player control stream нового renderer-bound AppState.
    pub(crate) fn attach_player_sender(&mut self, sender: player_core::PlayerCommandSender) {
        self.media_open.attach_player(sender);
    }

    /// Проверяет exact generation до применения будущего callback-а.
    pub(crate) fn validate_binding(
        &self,
        binding: PlaylistRuntimeBinding,
    ) -> Result<(), PlaylistBindingRejection> {
        match self.lifecycle {
            PlaylistRuntimeLifecycle::Bound(current) if current == binding => Ok(()),
            PlaylistRuntimeLifecycle::Bound(_) => Err(PlaylistBindingRejection::StaleGeneration),
            PlaylistRuntimeLifecycle::Suspended => Err(PlaylistBindingRejection::Suspended),
            PlaylistRuntimeLifecycle::ShuttingDown | PlaylistRuntimeLifecycle::Shutdown => {
                Err(PlaylistBindingRejection::ShuttingDown)
            }
        }
    }

    /// Неблокирующе опустошает process owner slots; UI payload здесь пока отсутствует.
    pub(crate) fn drain_owner_mailbox(&mut self) -> bool {
        let owner_changed = self.owner_receiver.drain().has_payload();
        let media_open_changed = self.media_open.drain();
        let dialog_changed = self.drain_playlist_file_dialog();
        owner_changed || media_open_changed || dialog_changed
    }

    /// Cheap-clone read-only snapshot для renderer-bound AppState/будущего UI port-а.
    #[allow(
        dead_code,
        reason = "read-only AppState attachment lands with playlist UI"
    )]
    pub(crate) fn playlist_view_snapshot(&self) -> Arc<PlaylistViewSnapshot> {
        static EMPTY_VIEW: LazyLock<Arc<PlaylistViewSnapshot>> =
            LazyLock::new(|| Arc::new(PlaylistViewSnapshot::initial(&PlaylistQueue::new())));
        self.controller
            .as_ref()
            .map(PlaylistController::view_snapshot)
            .unwrap_or_else(|| Arc::clone(&EMPTY_VIEW))
    }

    /// Создаёт renderer-bound attachment только для текущего exact binding-а.
    pub(crate) fn app_state_attachment(
        &self,
        binding: PlaylistRuntimeBinding,
    ) -> Result<PlaylistAppStateAttachment, PlaylistBindingRejection> {
        self.validate_binding(binding)?;
        Ok(PlaylistAppStateAttachment {
            binding,
            view_model: self.playlist_view_model(),
        })
    }

    /// Controller остаётся process owner-ом; mutable facade нужен orchestration layer-у.
    #[allow(
        dead_code,
        reason = "controller facade is consumed by Session 11B orchestration"
    )]
    pub(crate) fn playlist_controller(&self) -> Option<&PlaylistController> {
        self.controller.as_ref()
    }

    /// Закрывает admission и последовательно завершает owners в одном общем бюджете.
    ///
    /// Порядок намеренный: сначала отменяется player/media preparation, затем
    /// read/quarantine startup job, последним выполняется committed-only state flush.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> PlaylistTerminalShutdownOutcome {
        if matches!(self.lifecycle, PlaylistRuntimeLifecycle::Shutdown) {
            return PlaylistTerminalShutdownOutcome::AlreadyCompleted;
        }

        self.admission_open.store(false, Ordering::Release);
        self.lifecycle = PlaylistRuntimeLifecycle::ShuttingDown;
        self.removal_undo = None;
        self.replacement_confirmation.cancel();
        if let Some(controller) = self.controller.as_mut() {
            controller.release_detached_tombstone_for_shutdown();
        }
        self.discovery.begin_shutdown();

        let ui_interaction = self.ui_interaction.shutdown_until(deadline);
        let media_open = self.media_open.shutdown_until(deadline);
        let startup = self.startup.shutdown_until(deadline);
        let persistence = self
            .persistence
            .shutdown_until(self.controller.as_ref(), deadline);
        let report = PlaylistShutdownReport {
            ui_interaction,
            media_open,
            startup,
            persistence,
        };

        if report.requires_process_exit() {
            return PlaylistTerminalShutdownOutcome::ExitRequired(report);
        }
        self.lifecycle = PlaylistRuntimeLifecycle::Shutdown;
        if report.has_terminal_failure() {
            PlaylistTerminalShutdownOutcome::Failed(report)
        } else {
            PlaylistTerminalShutdownOutcome::Completed(report)
        }
    }

    #[cfg(test)]
    fn owner_ports(&self) -> PlaylistOwnerPorts {
        self.owner_ports.clone()
    }

    #[cfg(test)]
    fn load_gate(&self) -> PlaylistLoadGateState {
        self.load_gate
    }
}

impl PlaylistShutdownReport {
    fn requires_process_exit(self) -> bool {
        matches!(
            self.ui_interaction,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.media_open,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.startup,
            startup::PlaylistStartupShutdownOutcome::TimedOut
        ) || matches!(
            self.persistence,
            persistence::PlaylistPersistenceShutdownOutcome::TimedOut { .. }
        )
    }

    fn has_terminal_failure(self) -> bool {
        let ui_failed = matches!(
            self.ui_interaction,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let media_failed = matches!(
            self.media_open,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let startup_failed = matches!(
            self.startup,
            startup::PlaylistStartupShutdownOutcome::ThreadPanicked
        );
        let persistence_failed = match self.persistence {
            persistence::PlaylistPersistenceShutdownOutcome::WriterUnavailable { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::SnapshotCaptureFailed { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::ThreadPanicked(_) => true,
            persistence::PlaylistPersistenceShutdownOutcome::Completed(completion) => {
                shutdown_persistence_failed(completion.persistence)
            }
            persistence::PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::AlreadyCompleted
            | persistence::PlaylistPersistenceShutdownOutcome::TimedOut { .. } => false,
        };
        ui_failed || media_failed || startup_failed || persistence_failed
    }
}

fn shutdown_persistence_failed(persistence: playlist_state::ShutdownPersistenceOutcome) -> bool {
    match persistence {
        playlist_state::ShutdownPersistenceOutcome::NoCommittedSnapshot
        | playlist_state::ShutdownPersistenceOutcome::AlreadyDurable { .. } => false,
        playlist_state::ShutdownPersistenceOutcome::Attempted(report) => !matches!(
            report.outcome,
            playlist_state::SaveAttemptOutcome::FullWrite(
                playlist_state::AtomicWriteOutcome::Durable
            ) | playlist_state::SaveAttemptOutcome::DirectoryDurabilityRetry(
                playlist_state::DurabilityRetryOutcome::Durable
            )
        ),
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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};

    use super::*;

    struct CountingEmitter(AtomicUsize);

    impl WakeEmitter for CountingEmitter {
        fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn runtime() -> PlaylistRuntime {
        let emitter = Arc::new(CountingEmitter(AtomicUsize::new(0)));
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::new(AppWakeOwner::PlaylistRuntime, emitter));
        runtime.resolve_missing_state_for_test();
        runtime
    }

    #[test]
    fn suspend_resume_preserves_runtime_and_rejects_stale_binding() {
        let mut runtime = runtime();
        let initial_view = runtime.playlist_view_snapshot();
        let first = runtime.bind_resumed_app_state().unwrap();
        assert_eq!(runtime.validate_binding(first), Ok(()));

        runtime.suspend_app_state_binding();
        assert_eq!(
            runtime.validate_binding(first),
            Err(PlaylistBindingRejection::Suspended)
        );

        let second = runtime.bind_resumed_app_state().unwrap();
        assert_ne!(second, first);
        assert_eq!(
            runtime.validate_binding(first),
            Err(PlaylistBindingRejection::StaleGeneration)
        );
        assert_eq!(runtime.validate_binding(second), Ok(()));
        assert_eq!(
            runtime.load_gate(),
            PlaylistLoadGateState::Open(PlaylistLineagePersistence::Persistent)
        );

        let attachment = runtime
            .app_state_attachment(second)
            .expect("current binding attachment");
        assert_eq!(attachment.binding(), second);
        assert_eq!(attachment.view_model().revision(), initial_view.revision());
        assert!(matches!(
            runtime.app_state_attachment(first),
            Err(PlaylistBindingRejection::StaleGeneration)
        ));
    }

    #[test]
    fn owner_ports_survive_suspend_and_keep_mailbox_reachable() {
        let mut runtime = runtime();
        let ports = runtime.owner_ports();
        runtime.bind_resumed_app_state().unwrap();
        runtime.suspend_app_state_binding();

        assert!(ports.publish_progress());
        assert!(runtime.drain_owner_mailbox());
        assert!(!runtime.drain_owner_mailbox());
    }

    #[test]
    fn inline_url_draft_survives_views_and_confirmation_is_render_only() {
        let mut runtime = runtime();
        runtime.open_playlist_url_editor();
        runtime.update_playlist_url_draft("не URL с token=secret".to_string());

        assert!(runtime.submit_playlist_url_draft());
        let invalid = runtime.playlist_interaction_model();
        assert!(invalid.url_editor_open);
        assert_eq!(invalid.url_text, "не URL с token=secret");
        assert_eq!(
            invalid.url_safe_error.as_ref().map(|error| error.message()),
            Some("Введите корректный http(s) URL")
        );

        let sensitive = "https://user:password@media.example.test/video.mp4?token=secret";
        runtime.update_playlist_url_draft(sensitive.to_string());
        assert!(runtime.submit_playlist_url_draft());
        let confirmation = runtime
            .pending_playlist_confirmation()
            .expect("sensitive URL должен ждать typed decision");
        assert_eq!(runtime.playlist_interaction_model().url_text, sensitive);
        assert!(!format!("{confirmation:?}").contains("token=secret"));

        assert!(runtime.submit_playlist_url_draft());
        let stale_outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
            intent_id: confirmation.intent_id(),
            decision: QueueReplacementConfirmationDecision::Confirm,
        });
        runtime.finish_url_draft_after_confirmation(&stale_outcome);
        assert_eq!(runtime.playlist_interaction_model().url_text, sensitive);

        let current_confirmation = runtime
            .pending_playlist_confirmation()
            .expect("новый exact intent должен остаться pending");
        let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
            intent_id: current_confirmation.intent_id(),
            decision: QueueReplacementConfirmationDecision::Confirm,
        });
        runtime.finish_url_draft_after_confirmation(&outcome);

        let finished = runtime.playlist_interaction_model();
        assert!(!finished.url_editor_open);
        assert!(finished.url_text.is_empty());
        assert_eq!(runtime.controller.queue().len(), 1);
    }

    #[test]
    fn shutdown_is_bounded_idempotent_and_closes_admission() {
        let mut runtime = runtime();
        let ports = runtime.owner_ports();
        let deadline = ShutdownDeadline::after(Duration::from_secs(1));

        assert!(matches!(
            runtime.shutdown_until(deadline),
            PlaylistTerminalShutdownOutcome::Completed(_)
        ));
        assert!(!ports.publish_progress());
        assert_eq!(
            runtime.shutdown_until(deadline),
            PlaylistTerminalShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn media_open_timeout_requires_process_exit_without_collapsing_owner_outcomes() {
        let mut runtime = runtime();
        let ports = runtime.owner_ports();
        let report = PlaylistShutdownReport {
            media_open: ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 },
            ui_interaction: ProcessOwnerShutdownOutcome::Completed,
            startup: startup::PlaylistStartupShutdownOutcome::Completed,
            persistence: persistence::PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker {
                save_block: None,
            },
        };

        assert!(report.requires_process_exit());
        assert_eq!(
            report.media_open,
            ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
        );

        runtime.admission_open.store(false, Ordering::Release);
        runtime.lifecycle = PlaylistRuntimeLifecycle::ShuttingDown;
        assert!(!ports.publish_progress());
        assert!(runtime.bind_resumed_app_state().is_none());
    }
}
