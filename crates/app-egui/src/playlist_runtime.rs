//! Process-lifetime shell для будущего playlist controller/coordinators.
//!
//! Runtime владеет reusable media-open coordinator-ом, но по-прежнему не знает queue policy.
//! Он живёт в `AppShell`, а renderer-bound `AppState` получает короткоживущий binding
//! с новой generation и exact ordered player port после resume.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::app_wake::{AppWakePort, OwnerMailboxPublisher, OwnerMailboxReceiver, owner_mailbox};
use crate::media_open::MediaOpenCoordinator;

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
    Pending,
    /// Trusted load decision разрешил будущие domain commits.
    Ready,
}

/// Текущая lifecycle фаза process owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistRuntimeLifecycle {
    Suspended,
    Bound(PlaylistRuntimeBinding),
    ShuttingDown,
    Shutdown,
}

/// Deadline передаётся явно, чтобы shutdown API не прятал бесконечный wait.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlaylistShutdownDeadline(Instant);

impl PlaylistShutdownDeadline {
    /// Создаёт bounded deadline, выбранный process shutdown coordinator-ом.
    pub(crate) const fn at(deadline: Instant) -> Self {
        Self(deadline)
    }
}

/// Typed результат bounded idempotent shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistShutdownOutcome {
    /// Admission закрыт, а минимальный shell не держал blocking owners.
    Completed,
    /// Runtime уже был полностью закрыт предыдущим вызовом.
    AlreadyCompleted,
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

impl PlaylistOwnerPorts {
    /// Публикует latest progress только пока shutdown gate допускает работу.
    #[cfg(test)]
    fn publish_progress(&self) -> bool {
        if !self.admission_open.load(Ordering::Acquire) {
            return false;
        }
        self.publisher.publish_progress(PlaylistOwnerProgress);
        true
    }
}

/// Process-lifetime owner mechanism-ов до появления controller-а в Session 11A.
pub(crate) struct PlaylistRuntime {
    lifecycle_generation: PlaylistLifecycleGeneration,
    next_binding_generation: PlaylistBindingGeneration,
    lifecycle: PlaylistRuntimeLifecycle,
    #[allow(dead_code)] // Load decision wiring намеренно вне Session 10A.
    load_gate: PlaylistLoadGateState,
    admission_open: Arc<AtomicBool>,
    #[allow(dead_code)] // Поле удерживает worker-side ports process lifetime.
    owner_ports: PlaylistOwnerPorts,
    owner_receiver: OwnerMailboxReceiver<PlaylistOwnerProgress, PlaylistOwnerCompletion>,
    /// Process-lifetime reusable preparation/install mechanism Session 10C.
    media_open: MediaOpenCoordinator,
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
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        let media_open = MediaOpenCoordinator::new(wake_port.clone());
        let (publisher, owner_receiver) = owner_mailbox(wake_port);
        let admission_open = Arc::new(AtomicBool::new(true));
        Self {
            lifecycle_generation: PlaylistLifecycleGeneration(0),
            next_binding_generation: PlaylistBindingGeneration(0),
            lifecycle: PlaylistRuntimeLifecycle::Suspended,
            load_gate: PlaylistLoadGateState::Pending,
            owner_ports: PlaylistOwnerPorts {
                publisher,
                admission_open: admission_open.clone(),
            },
            admission_open,
            owner_receiver,
            media_open,
        }
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
    #[allow(dead_code)] // Используется будущими callbacks; focused tests фиксируют контракт сейчас.
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
        owner_changed || media_open_changed
    }

    /// Закрывает admission ровно один раз и не выдаёт скрытых обещаний о join/I/O.
    pub(crate) fn shutdown(
        &mut self,
        deadline: PlaylistShutdownDeadline,
    ) -> PlaylistShutdownOutcome {
        if matches!(self.lifecycle, PlaylistRuntimeLifecycle::Shutdown) {
            return PlaylistShutdownOutcome::AlreadyCompleted;
        }

        self.admission_open.store(false, Ordering::Release);
        self.lifecycle = PlaylistRuntimeLifecycle::ShuttingDown;
        self.media_open.shutdown();

        // В Session 10A blocking owners ещё нет: deadline уже является частью API,
        // но shell может подтвердить закрытие немедленно, не выполняя sleep/join.
        let _future_owner_deadline = deadline.0;

        self.lifecycle = PlaylistRuntimeLifecycle::Shutdown;
        PlaylistShutdownOutcome::Completed
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
    ) -> Result<player_core::MediaInstallRequestId, crate::media_open::MediaOpenCommandError> {
        self.media_open
            .stage_at_player(request_id, intent, video_resource_port)
    }

    /// Немедленно dispatch-ит matching authorization без второго buffer-а.
    pub(crate) fn authorize_ready_media_open(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<
        crate::media_open::AuthorizationDispatchResolution,
        crate::media_open::MediaOpenCommandError,
    > {
        self.media_open.authorize_ready(request_id)
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
        PlaylistRuntime::new(AppWakePort::new(AppWakeOwner::PlaylistRuntime, emitter))
    }

    #[test]
    fn suspend_resume_preserves_runtime_and_rejects_stale_binding() {
        let mut runtime = runtime();
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
        assert_eq!(runtime.load_gate(), PlaylistLoadGateState::Pending);
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
    fn shutdown_is_bounded_idempotent_and_closes_admission() {
        let mut runtime = runtime();
        let ports = runtime.owner_ports();
        let deadline = PlaylistShutdownDeadline::at(Instant::now() + Duration::from_secs(1));

        assert_eq!(
            runtime.shutdown(deadline),
            PlaylistShutdownOutcome::Completed
        );
        assert!(!ports.publish_progress());
        assert_eq!(
            runtime.shutdown(deadline),
            PlaylistShutdownOutcome::AlreadyCompleted
        );
    }
}
