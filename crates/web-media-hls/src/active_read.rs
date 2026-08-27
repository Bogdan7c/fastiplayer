//! HLS-private ownership active-read interruption и точного rollback descriptor-а.

use std::sync::{Arc, Mutex};

use media_core::{
    DemuxActiveReadInterrupter, DemuxActiveReadInterruptionCapability,
    DemuxActiveReadInterruptionPort, DemuxActiveReadInterruptionReason,
    DemuxActiveReadInterruptionResult,
};
use web_media_adaptive::{
    AdaptiveRestartableReadArmOutcome, AdaptiveRestartableReadAttempt,
    AdaptiveRestartableReadInterruption, AdaptiveRestartableReadInterruptionRequest,
};

use crate::plan::{HlsEpochPlan, HlsSegmentRestartCoordinate};

/// Stable controller одного HLS component lineage через все transactional replacements.
#[derive(Clone)]
pub(crate) struct HlsComponentActiveReadControl {
    adaptive: AdaptiveRestartableReadInterruption,
    port: DemuxActiveReadInterruptionPort,
}

impl HlsComponentActiveReadControl {
    /// Создаёт owner-private controller без URL, payload или parser state.
    pub(crate) fn new() -> Self {
        let adaptive = AdaptiveRestartableReadInterruption::new();
        let port =
            DemuxActiveReadInterruptionPort::new(Arc::new(HlsComponentActiveReadInterrupter {
                adaptive: adaptive.clone(),
            }));
        Self { adaptive, port }
    }

    /// Создаёт offside lifecycle нового parser/source attempt-а.
    pub(crate) fn new_epoch_lifecycle(&self, epoch: &HlsEpochPlan) -> HlsEpochActiveReadLifecycle {
        let first_media_restart = epoch
            .resources
            .iter()
            .find_map(|resource| resource.restart_segment);
        HlsEpochActiveReadLifecycle {
            adaptive: self.adaptive.clone(),
            shared: Arc::new(Mutex::new(HlsEpochActiveReadState {
                phase: HlsEpochActiveReadPhase::Offside,
                attempt: HlsCurrentReadAttempt::NotOpened,
                restart: match first_media_restart {
                    Some(coordinate) => HlsCurrentRestart::Media(coordinate),
                    None => HlsCurrentRestart::Unavailable,
                },
            })),
        }
    }

    /// Возвращает stable media-core port без утечки adaptive implementation type.
    pub(crate) fn capability(&self) -> DemuxActiveReadInterruptionCapability {
        DemuxActiveReadInterruptionCapability::Supported(self.port.clone())
    }
}

impl Default for HlsComponentActiveReadControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Neutral media-core adapter не принимает решений о receipt или parser commit-е.
struct HlsComponentActiveReadInterrupter {
    adaptive: AdaptiveRestartableReadInterruption,
}

impl DemuxActiveReadInterrupter for HlsComponentActiveReadInterrupter {
    fn request_active_read_interruption(
        &self,
        _reason: DemuxActiveReadInterruptionReason,
    ) -> DemuxActiveReadInterruptionResult {
        match self.adaptive.request_active_read_interruption() {
            AdaptiveRestartableReadInterruptionRequest::InterruptionRequested
            | AdaptiveRestartableReadInterruptionRequest::InterruptionAlreadyRequested => {
                DemuxActiveReadInterruptionResult::InterruptionRequestedRestartable
            }
            AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent => {
                DemuxActiveReadInterruptionResult::AlreadyQuiescent
            }
        }
    }
}

/// Source и component разделяют exact current attempt и restart coordinate после move в registry.
#[derive(Clone)]
pub(crate) struct HlsEpochActiveReadLifecycle {
    adaptive: AdaptiveRestartableReadInterruption,
    shared: Arc<Mutex<HlsEpochActiveReadState>>,
}

struct HlsEpochActiveReadState {
    phase: HlsEpochActiveReadPhase,
    attempt: HlsCurrentReadAttempt,
    restart: HlsCurrentRestart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsEpochActiveReadPhase {
    Offside,
    Committed,
}

enum HlsCurrentReadAttempt {
    NotOpened,
    Opened(AdaptiveRestartableReadAttempt),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsCurrentRestart {
    Unavailable,
    Media(HlsSegmentRestartCoordinate),
}

impl HlsEpochActiveReadLifecycle {
    /// Выделяет новый disarmed attempt для одного physical response body.
    pub(crate) fn new_resource_attempt(
        &self,
    ) -> Result<AdaptiveRestartableReadAttempt, HlsActiveReadError> {
        self.adaptive
            .new_attempt()
            .map_err(|_| HlsActiveReadError::AttemptIdentityExhausted)
    }

    /// Регистрирует уже открытый body; committed source arm-ит его до первого body read-а.
    pub(crate) fn register_opened_attempt(
        &self,
        attempt: AdaptiveRestartableReadAttempt,
    ) -> Result<(), HlsActiveReadError> {
        let should_arm = {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| HlsActiveReadError::StatePoisoned)?;
            state.attempt = HlsCurrentReadAttempt::Opened(attempt.clone());
            state.phase == HlsEpochActiveReadPhase::Committed
        };
        if should_arm {
            arm_current_attempt(&attempt)?;
        }
        Ok(())
    }

    /// Делает proven source authoritative только в момент outer commit-а.
    pub(crate) fn activate_committed(&self) -> Result<(), HlsActiveReadError> {
        let current_attempt = {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| HlsActiveReadError::StatePoisoned)?;
            state.phase = HlsEpochActiveReadPhase::Committed;
            match &state.attempt {
                HlsCurrentReadAttempt::NotOpened => None,
                HlsCurrentReadAttempt::Opened(attempt) => Some(attempt.clone()),
            }
        };
        if let Some(attempt) = current_attempt {
            arm_current_attempt(&attempt)?;
        }
        Ok(())
    }

    /// Обновляет descriptor перед публикацией `Begin` конкретного media resource-а.
    pub(crate) fn observe_media_restart(
        &self,
        coordinate: HlsSegmentRestartCoordinate,
    ) -> Result<(), HlsActiveReadError> {
        self.shared
            .lock()
            .map_err(|_| HlsActiveReadError::StatePoisoned)?
            .restart = HlsCurrentRestart::Media(coordinate);
        Ok(())
    }

    /// Снимает exact descriptor только после typed parser unwind-а.
    pub(crate) fn current_restart_coordinate(
        &self,
    ) -> Result<HlsSegmentRestartCoordinate, HlsActiveReadError> {
        let state = self
            .shared
            .lock()
            .map_err(|_| HlsActiveReadError::StatePoisoned)?;
        match state.restart {
            HlsCurrentRestart::Media(coordinate) => Ok(coordinate),
            HlsCurrentRestart::Unavailable => Err(HlsActiveReadError::RestartUnavailable),
        }
    }
}

fn arm_current_attempt(attempt: &AdaptiveRestartableReadAttempt) -> Result<(), HlsActiveReadError> {
    match attempt.arm_as_current() {
        AdaptiveRestartableReadArmOutcome::Armed
        | AdaptiveRestartableReadArmOutcome::AlreadyCurrent => Ok(()),
        AdaptiveRestartableReadArmOutcome::StaleAttemptRejected => {
            Err(HlsActiveReadError::StaleAttemptRejected)
        }
    }
}

/// Operational ошибки HLS-private active-read ownership без locator-а или secret material.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HlsActiveReadError {
    #[error("HLS active-read attempt identity space exhausted")]
    AttemptIdentityExhausted,
    #[error("HLS active-read shared state poisoned")]
    StatePoisoned,
    #[error("HLS active-read attempt was superseded before commit")]
    StaleAttemptRejected,
    #[error("HLS current resource has no restartable media coordinate")]
    RestartUnavailable,
}
