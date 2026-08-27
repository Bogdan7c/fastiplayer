//! Passive vocabulary staged video pipeline protocol-а.
//!
//! Модуль описывает identities, payload-ы и наблюдаемые outcomes. Mutation slot-а,
//! release обеих resource halves, post-Installed commit и Drop остаются у parent owner-а.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use player_core::MediaInstallRequestId;
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendConfigurationError,
};

use super::resource_driver::{
    CandidateVideoPipelineDescriptor, CandidateVideoPipelinePreparationError,
};

/// Exact generation renderer-а, к которому привязаны candidate GPU resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RendererGeneration(NonZeroU64);

/// Process-local allocator не позволяет двум renderer lifetimes разделить identity.
static NEXT_RENDERER_GENERATION: AtomicU64 = AtomicU64::new(1);

impl RendererGeneration {
    /// Выдаёт новую renderer identity для resume/recreation owner-а.
    #[must_use]
    pub(crate) fn new_unique() -> Self {
        // Relaxed достаточно: allocator задаёт identity, а не публикует renderer resources.
        let raw = NEXT_RENDERER_GENERATION.fetch_add(1, Ordering::Relaxed);
        let generation =
            NonZeroU64::new(raw).expect("renderer generation identity space exhausted");
        Self(generation)
    }

    /// Создаёт generation из explicit non-zero значения owner-а.
    #[must_use]
    pub(crate) const fn from_non_zero(generation: NonZeroU64) -> Self {
        // NonZeroU64 не допускает ambiguous default/stale generation zero.
        Self(generation)
    }

    /// Возвращает числовое значение для diagnostics и deterministic tests.
    #[must_use]
    pub(crate) const fn get(self) -> u64 {
        // Renderer generation не содержит pointer или platform handle.
        self.0.get()
    }
}

/// App-owned renderer half одного admitted candidate-а.
pub(super) struct StagedVideoPipelineCandidate<Materializer, SubmissionBinding> {
    /// Exact media install request связывает обе split halves.
    pub(super) request_id: MediaInstallRequestId,

    /// Exact renderer generation запрещает commit stale GPU resources.
    pub(super) renderer_generation: RendererGeneration,

    /// Backend/materializer descriptor остаётся доступен после split handoff.
    pub(super) descriptor: CandidateVideoPipelineDescriptor,

    /// Canonical backend ID проверяет reply status против фактического decoder-а.
    pub(super) backend_id: String,

    /// Player status переводит candidate только Awaiting -> StreamConfigured.
    pub(super) state: StagedVideoPipelineCandidateState,

    /// Renderer-bound materializer ещё не является active.
    pub(super) materializer: Materializer,

    /// Candidate submission binding ещё не является active.
    pub(super) submission_binding: SubmissionBinding,
}

/// Минимальное app-side состояние без player ReadyToCommit state machine 00C1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StagedVideoPipelineCandidateState {
    /// Detached backend half передан player owner-у и ждёт typed status.
    AwaitingPlayer,

    /// Player подтвердил successful stream configuration до будущего Installed barrier.
    StreamConfigured,

    /// Matching Installed уже принят; lifecycle обязан завершить pointer commit.
    PostInstalledCommitRequired,
}

/// Bounded diagnostics без hidden retry/spin counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StagedVideoPipelineCandidateDiagnostics {
    /// Число успешно admitted resource sets.
    pub(crate) admitted: u64,

    /// Число отказов admission из-за occupied candidate/terminal slot-а.
    pub(crate) admission_backpressure: u64,

    /// Число fallible preparation failures до split success.
    pub(crate) preparation_failures: u64,

    /// Число terminal cancellations до commit barrier.
    pub(crate) cancellations: u64,

    /// Число infallible app pointer commits после matching Installed.
    pub(crate) commits: u64,
}

/// Один lossless terminal outcome admitted candidate-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StagedVideoPipelineCandidateTerminalOutcome {
    /// Candidate creation закончился typed failure до split handoff.
    PreparationFailed {
        /// Exact request ID rejected resource set-а.
        request_id: MediaInstallRequestId,

        /// Renderer generation, для которого выполнялась preparation.
        renderer_generation: RendererGeneration,

        /// Typed preparation failure.
        error: CandidateVideoPipelinePreparationError,
    },

    /// Player-side stream configuration закончилась typed failure.
    ConfigurationFailed {
        /// Exact request ID configured candidate-а.
        request_id: MediaInstallRequestId,

        /// Typed neutral configuration error.
        error: DetachedVideoBackendConfigurationError,
    },

    /// Candidate обеих половин terminal-cancelled до commit barrier.
    Cancelled {
        /// Exact cancelled request ID.
        request_id: MediaInstallRequestId,

        /// Distinct cancellation cause.
        cause: DetachedVideoBackendCandidateCancellationCause,
    },

    /// Matching Installed завершил infallible app pointer commit.
    Installed {
        /// Exact committed request ID.
        request_id: MediaInstallRequestId,

        /// Exact committed renderer generation.
        renderer_generation: RendererGeneration,
    },
}

/// Ошибка matching status/Installed без mutation чужого candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedVideoPipelineCandidateMatchError {
    /// Slot не содержит admitted candidate.
    NoCandidate,

    /// Reply/status принадлежит другому request-у.
    RequestMismatch,

    /// Candidate GPU resources принадлежат stale renderer generation.
    RendererGenerationMismatch,

    /// Player сообщил backend ID, не совпадающий с prepared pair.
    BackendMismatch,

    /// Candidate ещё не подтвердил stream configuration.
    NotStreamConfigured,

    /// Duplicate configured status нарушил ordered protocol.
    AlreadyStreamConfigured,

    /// Matching Installed barrier уже принят, поэтому pre-barrier cancel запрещён.
    PostInstalledCommitRequired,
}

/// Fatal protocol invariant после принятого player `Installed` barrier-а.
///
/// До `Installed` matching error остаётся обычным candidate rejection. После
/// `Installed` player ownership уже переключён, поэтому отсутствие exact app half-а
/// нельзя маскировать recoverable install failure или попыткой rollback-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PostInstalledVideoPipelineInvariantViolation {
    /// Точная причина нарушения split-resource agreement.
    pub(super) match_error: StagedVideoPipelineCandidateMatchError,
}

impl PostInstalledVideoPipelineInvariantViolation {
    /// Возвращает typed причину для fatal diagnostics owner-а.
    #[must_use]
    pub(crate) const fn match_error(self) -> StagedVideoPipelineCandidateMatchError {
        self.match_error
    }
}

/// Ошибка применения player status после обязательного terminal cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedVideoPipelineCandidateStatusError {
    /// Status не совпал с current candidate или нарушил ordered phase.
    Match(StagedVideoPipelineCandidateMatchError),

    /// Cancel dispatch потерял port, но обе локально доступные halves освобождены.
    PortDisconnected {
        /// Исходная matching-причина остаётся доступна diagnostics owner-у.
        match_error: StagedVideoPipelineCandidateMatchError,
    },
}

/// Результат pre-barrier cancel dispatch после обязательного app-half release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedVideoPipelineCandidateCancelError {
    /// Request не совпал с admitted candidate и ничего не изменилось.
    Match(StagedVideoPipelineCandidateMatchError),

    /// Port disconnect стал terminal cause; app half всё равно освобождён.
    PortDisconnected,
}
