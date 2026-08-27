//! Deferred proof начальной HLS VOD позиции на границе opener → app.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use media_core::{DemuxSeekResult, MediaTime};
use web_media_transport_api::SourceGeneration;

use crate::start::HlsResolvedVodStartIntent;

/// Authoritative результат target-aware open после доказанного RAP/audio packet-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsInitialPositionProof {
    generation: SourceGeneration,
    target_position: MediaTime,
    result: DemuxSeekResult,
}

impl HlsInitialPositionProof {
    /// Source generation, для которой worker доказал initial landing.
    #[must_use]
    pub const fn generation(self) -> SourceGeneration {
        self.generation
    }

    /// Точная caller-owned restore target, а не начало manifest segment-а.
    #[must_use]
    pub const fn target_position(self) -> MediaTime {
        self.target_position
    }

    /// Container-proven decode-safe landing result.
    #[must_use]
    pub const fn demux_seek_result(self) -> DemuxSeekResult {
        self.result
    }
}

/// Capability явно отличает обычный beginning open от deferred restore proof-а.
#[derive(Clone)]
pub enum HlsInitialPositionProofCapability {
    /// Beginning open не создаёт скрытого position contract-а.
    NotRequested,
    /// Restore proof появится только после успешного deferred component open-а.
    Deferred(HlsInitialPositionProofPort),
}

impl fmt::Debug for HlsInitialPositionProofCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRequested => formatter.write_str("NotRequested"),
            Self::Deferred(port) => formatter.debug_tuple("Deferred").field(port).finish(),
        }
    }
}

/// Cloneable opaque consumer handle одного deferred initial-position proof-а.
#[derive(Clone)]
pub struct HlsInitialPositionProofPort {
    generation: SourceGeneration,
    shared: Arc<HlsInitialPositionProofShared>,
}

impl HlsInitialPositionProofPort {
    /// Забирает proof ровно один раз и fail-closed проверяет exact source generation.
    #[must_use]
    pub fn take_for_generation(
        &self,
        expected_generation: SourceGeneration,
    ) -> HlsInitialPositionProofTakeOutcome {
        if expected_generation != self.generation {
            return HlsInitialPositionProofTakeOutcome::StaleGeneration;
        }
        let mut state = self.shared.lock_state();
        match *state {
            HlsInitialPositionProofState::Pending => HlsInitialPositionProofTakeOutcome::Pending,
            HlsInitialPositionProofState::Ready(proof) => {
                *state = HlsInitialPositionProofState::Taken;
                HlsInitialPositionProofTakeOutcome::Ready(proof)
            }
            HlsInitialPositionProofState::Failed => HlsInitialPositionProofTakeOutcome::Failed,
            HlsInitialPositionProofState::Taken => HlsInitialPositionProofTakeOutcome::AlreadyTaken,
        }
    }
}

impl fmt::Debug for HlsInitialPositionProofPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsInitialPositionProofPort")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Неблокирующий typed результат чтения deferred proof-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsInitialPositionProofTakeOutcome {
    /// Deferred worker ещё не закончил authoritative initial open.
    Pending,
    /// Exact proof передан consumer-у и больше не будет выдан повторно.
    Ready(HlsInitialPositionProof),
    /// Другой clone уже забрал единственный proof.
    AlreadyTaken,
    /// Deferred initial open завершился terminal failure до публикации proof-а.
    Failed,
    /// Consumer относится к другой source generation и не может наблюдать state.
    StaleGeneration,
}

/// Producer half остаётся HLS-private и живёт только внутри deferred worker closure.
#[derive(Clone)]
pub(crate) struct HlsInitialPositionProofPublisher {
    mode: HlsInitialPositionProofPublisherMode,
}

#[derive(Clone)]
enum HlsInitialPositionProofPublisherMode {
    Disabled,
    Enabled {
        generation: SourceGeneration,
        target_position: MediaTime,
        shared: Arc<HlsInitialPositionProofShared>,
    },
}

impl HlsInitialPositionProofPublisher {
    /// Создаёт capability/publisher pair до запуска deferred worker-а.
    pub(crate) fn for_start(
        start: HlsResolvedVodStartIntent,
        generation: SourceGeneration,
    ) -> (HlsInitialPositionProofCapability, Self) {
        match start {
            HlsResolvedVodStartIntent::Beginning => (
                HlsInitialPositionProofCapability::NotRequested,
                Self {
                    mode: HlsInitialPositionProofPublisherMode::Disabled,
                },
            ),
            HlsResolvedVodStartIntent::Restore(target_position) => {
                let shared = Arc::new(HlsInitialPositionProofShared::default());
                (
                    HlsInitialPositionProofCapability::Deferred(HlsInitialPositionProofPort {
                        generation,
                        shared: Arc::clone(&shared),
                    }),
                    Self {
                        mode: HlsInitialPositionProofPublisherMode::Enabled {
                            generation,
                            target_position,
                            shared,
                        },
                    },
                )
            }
        }
    }

    /// Публикует только exact validated result окончательно выбранного candidate-а.
    pub(crate) fn publish(
        &self,
        result: DemuxSeekResult,
    ) -> Result<(), HlsInitialPositionProofPublishError> {
        let HlsInitialPositionProofPublisherMode::Enabled {
            generation,
            target_position,
            shared,
        } = &self.mode
        else {
            return Err(HlsInitialPositionProofPublishError::UnexpectedPositionedBeginning);
        };
        if result.requested_position != *target_position {
            return Err(HlsInitialPositionProofPublishError::RequestedTargetMismatch);
        }
        let mut state = shared.lock_state();
        if !matches!(*state, HlsInitialPositionProofState::Pending) {
            return Err(HlsInitialPositionProofPublishError::TerminalStateAlreadySet);
        }
        *state = HlsInitialPositionProofState::Ready(HlsInitialPositionProof {
            generation: *generation,
            target_position: *target_position,
            result,
        });
        Ok(())
    }

    /// Beginning evidence допустим только для open-а без restore capability.
    pub(crate) fn publish_beginning(&self) -> Result<(), HlsInitialPositionProofPublishError> {
        match self {
            Self {
                mode: HlsInitialPositionProofPublisherMode::Disabled,
            } => Ok(()),
            Self {
                mode: HlsInitialPositionProofPublisherMode::Enabled { .. },
            } => Err(HlsInitialPositionProofPublishError::MissingRestoreProof),
        }
    }

    /// Settles общий open failure; отдельный rejected candidate этот метод не вызывает.
    pub(crate) fn publish_failure(&self) {
        let HlsInitialPositionProofPublisherMode::Enabled { shared, .. } = &self.mode else {
            return;
        };
        let mut state = shared.lock_state();
        if matches!(*state, HlsInitialPositionProofState::Pending) {
            *state = HlsInitialPositionProofState::Failed;
        }
    }
}

/// Нарушение owner-local proof invariant делает весь deferred open terminal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HlsInitialPositionProofPublishError {
    #[error("beginning HLS open unexpectedly produced positioned restore proof")]
    UnexpectedPositionedBeginning,
    #[error("restore HLS open completed without authoritative positioned proof")]
    MissingRestoreProof,
    #[error("HLS initial proof requested target does not match start intent")]
    RequestedTargetMismatch,
    #[error("HLS initial proof terminal state was already settled")]
    TerminalStateAlreadySet,
}

#[derive(Default)]
struct HlsInitialPositionProofShared {
    state: Mutex<HlsInitialPositionProofState>,
}

impl HlsInitialPositionProofShared {
    fn lock_state(&self) -> MutexGuard<'_, HlsInitialPositionProofState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Default)]
enum HlsInitialPositionProofState {
    #[default]
    Pending,
    Ready(HlsInitialPositionProof),
    Failed,
    Taken,
}
