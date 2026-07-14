//! Process-lifetime shell для будущего playlist controller/coordinators.
//!
//! Этот модуль пока не реализует queue policy и media open. Он закрепляет только
//! правильный уровень ownership: runtime живёт в `AppShell`, а renderer-bound
//! `AppState` получает короткоживущий binding с новой generation после resume.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::app_wake::{AppWakePort, OwnerMailboxPublisher, OwnerMailboxReceiver, owner_mailbox};

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

/// Минимальный process-lifetime owner до появления controller-а в Session 11A.
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
}

impl PlaylistRuntime {
    /// Создаёт runtime один раз вместе с `AppShell`, до любого `AppState`.
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
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
        }
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
    pub(crate) fn drain_owner_mailbox(&self) -> bool {
        self.owner_receiver.drain().has_payload()
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
