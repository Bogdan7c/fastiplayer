//! Cooperative typed cancellation и reversible D62 freeze.

use std::sync::{Arc, Mutex};

use source_core::CancellationToken;

/// Причина terminal cancellation; stale result остаётся отдельной app-проверкой.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryCancellationCause {
    /// Пользователь нажал Cancel.
    UserCancelled,
    /// Более новый intent заменил job.
    Superseded,
    /// Transport Stop отменил ожидающую работу.
    TransportStop,
    /// Structural revision сделала future apply недопустимым.
    StructuralInvalidation,
    /// Renderer/app lifecycle временно suspended.
    LifecycleSuspended,
    /// Process-lifetime owner начал shutdown.
    LifecycleShutdown,
}

#[derive(Debug, Default)]
struct CancellationState {
    cause: Option<DiscoveryCancellationCause>,
    frozen: bool,
}

/// Cloneable job-owned control, объединяющий typed cause и probe token.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryCancellation {
    state: Arc<Mutex<CancellationState>>,
    probe_token: CancellationToken,
}

impl DiscoveryCancellation {
    /// Линеаризует первую cancellation cause и будит blocking probe boundary token-ом.
    pub fn cancel(&self, cause: DiscoveryCancellationCause) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.cause.is_some() {
            return false;
        }
        state.cause = Some(cause);
        drop(state);
        self.probe_token.cancel();
        true
    }

    /// Замораживает admission/result apply без разрушения buffered ownership.
    pub fn freeze(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.cause.is_some() || state.frozen {
            return false;
        }
        state.frozen = true;
        true
    }

    /// Снимает reversible settings-stage freeze с того же job scope.
    pub fn resume(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.cause.is_some() || !state.frozen {
            return false;
        }
        state.frozen = false;
        true
    }

    /// Возвращает первую terminal cause.
    #[must_use]
    pub fn cause(&self) -> Option<DiscoveryCancellationCause> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cause
    }

    /// Проверяет reversible admission freeze.
    #[must_use]
    pub fn is_frozen(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .frozen
    }

    /// Передаёт single-file probe owner-у общий cooperative token.
    #[must_use]
    pub fn probe_token(&self) -> &CancellationToken {
        &self.probe_token
    }
}
