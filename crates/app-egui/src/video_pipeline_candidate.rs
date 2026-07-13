//! Renderer-bound candidate video pipeline resources для strong media install.
//!
//! Session 00C создаёт bounded resource boundary, а Session 00C1 использует его
//! после player `Installed` только для exact infallible pointer commit-а.

#![allow(
    dead_code,
    reason = "Session 00C1 validates the boundary before later coordinator call-site migration"
)]

use std::num::NonZeroU64;

use player_core::MediaInstallRequestId;
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendResourceError,
    DetachedVideoBackendResourcePort,
};

use crate::video_pipeline_selector::{VideoBackendKind, VideoPipelinePlan};

/// Exact generation renderer-а, к которому привязаны candidate GPU resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RendererGeneration(NonZeroU64);

impl RendererGeneration {
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

mod resource_driver;

use resource_driver::{
    CandidateVideoPipelineDescriptor, CandidateVideoPipelinePreparationError,
    CandidateVideoPipelineResourceDriver,
};

/// App-owned renderer half одного admitted candidate-а.
struct StagedVideoPipelineCandidate<Materializer, SubmissionBinding> {
    /// Exact media install request связывает обе split halves.
    request_id: MediaInstallRequestId,

    /// Exact renderer generation запрещает commit stale GPU resources.
    renderer_generation: RendererGeneration,

    /// Backend/materializer descriptor остаётся доступен после split handoff.
    descriptor: CandidateVideoPipelineDescriptor,

    /// Canonical backend ID проверяет reply status против фактического decoder-а.
    backend_id: String,

    /// Player status переводит candidate только Awaiting -> StreamConfigured.
    state: StagedVideoPipelineCandidateState,

    /// Renderer-bound materializer ещё не является active.
    materializer: Materializer,

    /// Candidate submission binding ещё не является active.
    submission_binding: SubmissionBinding,
}

/// Минимальное app-side состояние без player ReadyToCommit state machine 00C1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedVideoPipelineCandidateState {
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
        error: video_backend_api::DetachedVideoBackendConfigurationError,
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
    match_error: StagedVideoPipelineCandidateMatchError,
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

/// Ровно один candidate slot и один terminal outcome slot.
pub(crate) struct StagedVideoPipelineCandidateSlot<Materializer, SubmissionBinding> {
    /// `Some` означает единственный admitted app half.
    candidate: Option<StagedVideoPipelineCandidate<Materializer, SubmissionBinding>>,

    /// Terminal outcome не coalesce-ится и drain-ится exactly once.
    terminal_outcome: Option<StagedVideoPipelineCandidateTerminalOutcome>,

    /// Bounded accounting не управляет retry policy.
    diagnostics: StagedVideoPipelineCandidateDiagnostics,
}

impl<Materializer, SubmissionBinding>
    StagedVideoPipelineCandidateSlot<Materializer, SubmissionBinding>
{
    /// Создаёт пустой slot без backend pool или hidden queue.
    #[must_use]
    pub(crate) const fn new() -> Self {
        // Candidate и terminal slot начинают пустыми.
        Self {
            candidate: None,
            terminal_outcome: None,
            diagnostics: StagedVideoPipelineCandidateDiagnostics {
                admitted: 0,
                admission_backpressure: 0,
                preparation_failures: 0,
                cancellations: 0,
                commits: 0,
            },
        }
    }

    /// Создаёт и stage-ит один pair либо возвращает request-correlated typed reply.
    pub(crate) fn prepare_and_stage<Driver>(
        &mut self,
        request_id: MediaInstallRequestId,
        renderer_generation: RendererGeneration,
        plan: VideoPipelinePlan,
        driver: &mut Driver,
    ) -> DetachedVideoBackendReply<MediaInstallRequestId>
    where
        Driver: CandidateVideoPipelineResourceDriver<
                Materializer = Materializer,
                SubmissionBinding = SubmissionBinding,
            >,
    {
        // Новый backend нельзя запускать, пока occupied candidate/terminal не обработан.
        if self.candidate.is_some() || self.terminal_outcome.is_some() {
            // Backpressure считается без retry/spin внутри owner-а.
            self.diagnostics.admission_backpressure =
                self.diagnostics.admission_backpressure.saturating_add(1);
            // Failed reply остаётся exact request-correlated.
            return DetachedVideoBackendReply::unavailable(
                request_id,
                DetachedVideoBackendResourceError::AdmissionBackpressure {
                    reason: "candidate slot or terminal outcome is occupied".to_owned(),
                },
            );
        }

        // Descriptor вычисляется до fallible work и фиксирует matching pair.
        let descriptor = CandidateVideoPipelineDescriptor::from_plan(plan);
        // Driver не получает active pointers и не способен их мутировать.
        let resources = match driver.prepare_candidate_resources(plan) {
            Ok(resources) => resources,
            Err(error) => {
                // Preparation failure публикуется в единственный lossless terminal slot.
                self.diagnostics.preparation_failures =
                    self.diagnostics.preparation_failures.saturating_add(1);
                // Backend ID здесь является stable plan label, а не config knob.
                let backend_label = plan.diagnostic_label();
                // Neutral reply сохраняет resource-exhausted/unavailable distinction.
                let resource_error = error.to_resource_error(backend_label);
                // Admitted work terminal outcome не теряется до explicit drain.
                self.terminal_outcome = Some(
                    StagedVideoPipelineCandidateTerminalOutcome::PreparationFailed {
                        request_id,
                        renderer_generation,
                        error,
                    },
                );
                // Player получает matching failure и не запускает fallback backend.
                return DetachedVideoBackendReply::unavailable(request_id, resource_error);
            }
        };

        // Canonical ID читается до передачи единственного detached owner-а.
        let backend_id = resources.detached_backend.backend_id().to_owned();
        // App slot сохраняет только renderer half и matching metadata.
        self.candidate = Some(StagedVideoPipelineCandidate {
            request_id,
            renderer_generation,
            descriptor,
            backend_id,
            state: StagedVideoPipelineCandidateState::AwaitingPlayer,
            materializer: resources.materializer,
            submission_binding: resources.submission_binding,
        });
        // Successful admission увеличивает bounded counter ровно один раз.
        self.diagnostics.admitted = self.diagnostics.admitted.saturating_add(1);

        // Player half покидает app owner только в exact-correlated reply.
        DetachedVideoBackendReply::available(request_id, resources.detached_backend)
    }

    /// Применяет matching player status и terminal-cancel-ит stale/mismatched pair.
    pub(crate) fn record_player_status<Port>(
        &mut self,
        status: DetachedVideoBackendCandidateStatus<MediaInstallRequestId>,
        current_renderer_generation: RendererGeneration,
        port: &mut Port,
    ) -> Result<(), StagedVideoPipelineCandidateStatusError>
    where
        Port: DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>,
    {
        // Status без candidate не создаёт новый state задним числом.
        let Some(candidate) = self.candidate.as_ref() else {
            return Err(StagedVideoPipelineCandidateStatusError::Match(
                StagedVideoPipelineCandidateMatchError::NoCandidate,
            ));
        };
        // Stale reply другого request-а не может очистить текущий candidate.
        if status.request_id() != &candidate.request_id {
            return Err(StagedVideoPipelineCandidateStatusError::Match(
                StagedVideoPipelineCandidateMatchError::RequestMismatch,
            ));
        }
        // Renderer mismatch terminal-cancel-ит exact candidate до status apply.
        if candidate.renderer_generation != current_renderer_generation {
            // Player half получает exact stale-generation cancellation.
            let cancel_result = port.cancel_candidate(
                candidate.request_id,
                DetachedVideoBackendCandidateCancellationCause::StaleRendererGeneration,
            );
            // Disconnect остаётся отдельным terminal cause и не игнорируется.
            let terminal_cause = if cancel_result.is_ok() {
                DetachedVideoBackendCandidateCancellationCause::StaleRendererGeneration
            } else {
                DetachedVideoBackendCandidateCancellationCause::Disconnected
            };
            // App half освобождается независимо от port disconnect.
            self.finish_cancelled(terminal_cause);
            // Caller получает как match rejection, так и возможный port disconnect.
            return if cancel_result.is_ok() {
                Err(StagedVideoPipelineCandidateStatusError::Match(
                    StagedVideoPipelineCandidateMatchError::RendererGenerationMismatch,
                ))
            } else {
                Err(StagedVideoPipelineCandidateStatusError::PortDisconnected {
                    match_error: StagedVideoPipelineCandidateMatchError::RendererGenerationMismatch,
                })
            };
        }

        // Matching status сохраняет distinct success/failure/cancel semantics.
        match status {
            DetachedVideoBackendCandidateStatus::StreamConfigured {
                request_id: _,
                backend_id,
            } => {
                // Backend mismatch запрещает смешивать halves разных concrete plans.
                if backend_id != candidate.backend_id {
                    // Player configured half должен быть освобождён по exact request ID.
                    let cancel_result = port.cancel_candidate(
                        candidate.request_id,
                        DetachedVideoBackendCandidateCancellationCause::Requested,
                    );
                    // Disconnect не должен исчезнуть за backend mismatch diagnostics.
                    let terminal_cause = if cancel_result.is_ok() {
                        DetachedVideoBackendCandidateCancellationCause::Requested
                    } else {
                        DetachedVideoBackendCandidateCancellationCause::Disconnected
                    };
                    // App half terminal-cancel-ится и не становится active.
                    self.finish_cancelled(terminal_cause);
                    // Caller видит exact matching error и transport state.
                    return if cancel_result.is_ok() {
                        Err(StagedVideoPipelineCandidateStatusError::Match(
                            StagedVideoPipelineCandidateMatchError::BackendMismatch,
                        ))
                    } else {
                        Err(StagedVideoPipelineCandidateStatusError::PortDisconnected {
                            match_error: StagedVideoPipelineCandidateMatchError::BackendMismatch,
                        })
                    };
                }
                // Duplicate success не меняет уже prepared state.
                if candidate.state != StagedVideoPipelineCandidateState::AwaitingPlayer {
                    return Err(StagedVideoPipelineCandidateStatusError::Match(
                        StagedVideoPipelineCandidateMatchError::AlreadyStreamConfigured,
                    ));
                }
                // Единственная mutation переводит app half в configured-ready marker.
                self.candidate
                    .as_mut()
                    .expect("candidate was validated above")
                    .state = StagedVideoPipelineCandidateState::StreamConfigured;
                Ok(())
            }
            DetachedVideoBackendCandidateStatus::ConfigurationFailed { request_id, error } => {
                // Player уже освободил failed decoder half; app освобождает matching half.
                let _candidate = self
                    .candidate
                    .take()
                    .expect("candidate was validated above");
                // Typed failure публикуется ровно в один terminal slot.
                self.publish_terminal(
                    StagedVideoPipelineCandidateTerminalOutcome::ConfigurationFailed {
                        request_id,
                        error,
                    },
                );
                Ok(())
            }
            DetachedVideoBackendCandidateStatus::Cancelled { request_id, cause } => {
                // Player уже освободил decoder half; app half освобождается через take/drop.
                let _candidate = self
                    .candidate
                    .take()
                    .expect("candidate was validated above");
                // Cancellation accounting не смешивается с configuration failure.
                self.diagnostics.cancellations = self.diagnostics.cancellations.saturating_add(1);
                // Exact typed cancellation сохраняется losslessly.
                self.publish_terminal(StagedVideoPipelineCandidateTerminalOutcome::Cancelled {
                    request_id,
                    cause,
                });
                Ok(())
            }
        }
    }

    /// Terminal-cancel-ит exact candidate до barrier и освобождает обе split halves.
    pub(crate) fn cancel_pre_barrier<Port>(
        &mut self,
        request_id: MediaInstallRequestId,
        cause: DetachedVideoBackendCandidateCancellationCause,
        port: &mut Port,
    ) -> Result<(), StagedVideoPipelineCandidateCancelError>
    where
        Port: DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>,
    {
        // Cancel без candidate не создаёт synthetic terminal outcome.
        let Some(candidate) = self.candidate.as_ref() else {
            return Err(StagedVideoPipelineCandidateCancelError::Match(
                StagedVideoPipelineCandidateMatchError::NoCandidate,
            ));
        };
        // Stale request не может отменить новый admitted candidate.
        if candidate.request_id != request_id {
            return Err(StagedVideoPipelineCandidateCancelError::Match(
                StagedVideoPipelineCandidateMatchError::RequestMismatch,
            ));
        }
        // После matching Installed lifecycle уже не может отменить candidate.
        if candidate.state == StagedVideoPipelineCandidateState::PostInstalledCommitRequired {
            return Err(StagedVideoPipelineCandidateCancelError::Match(
                StagedVideoPipelineCandidateMatchError::PostInstalledCommitRequired,
            ));
        }

        // Player direction получает cancellation до app-half drop.
        let cancel_result = port.cancel_candidate(request_id, cause);
        // Disconnect становится отдельной terminal cause, не игнорируя ошибку silently.
        let terminal_cause = if cancel_result.is_ok() {
            cause
        } else {
            DetachedVideoBackendCandidateCancellationCause::Disconnected
        };
        // App half всегда освобождается ровно один раз через owned candidate drop.
        self.finish_cancelled(terminal_cause);

        // Caller видит disconnect, хотя local cleanup уже завершён.
        cancel_result.map_err(|DetachedVideoBackendPortError| {
            StagedVideoPipelineCandidateCancelError::PortDisconnected
        })
    }

    /// Валидирует matching Installed и отдаёт token только для pointer-only commit-а.
    pub(crate) fn prepare_post_installed_commit(
        &mut self,
        request_id: MediaInstallRequestId,
        current_renderer_generation: RendererGeneration,
    ) -> Result<
        PreparedPostInstalledVideoPipelineCommit<'_, Materializer, SubmissionBinding>,
        PostInstalledVideoPipelineInvariantViolation,
    > {
        // Installed без admitted candidate является stale protocol event-ом.
        let Some(candidate) = self.candidate.as_ref() else {
            return Err(PostInstalledVideoPipelineInvariantViolation {
                match_error: StagedVideoPipelineCandidateMatchError::NoCandidate,
            });
        };
        // Installed другого request-а не меняет current candidate.
        if candidate.request_id != request_id {
            return Err(PostInstalledVideoPipelineInvariantViolation {
                match_error: StagedVideoPipelineCandidateMatchError::RequestMismatch,
            });
        }
        // Stale renderer resources нельзя переместить в active pointers.
        if candidate.renderer_generation != current_renderer_generation {
            return Err(PostInstalledVideoPipelineInvariantViolation {
                match_error: StagedVideoPipelineCandidateMatchError::RendererGenerationMismatch,
            });
        }
        // Player configuration обязана завершиться до Installed pointer commit.
        if candidate.state == StagedVideoPipelineCandidateState::AwaitingPlayer {
            return Err(PostInstalledVideoPipelineInvariantViolation {
                match_error: StagedVideoPipelineCandidateMatchError::NotStreamConfigured,
            });
        }

        // Barrier marker устанавливается до извлечения pointers в linear commit token.
        self.candidate
            .as_mut()
            .expect("candidate was validated above")
            .state = StagedVideoPipelineCandidateState::PostInstalledCommitRequired;

        // Candidate извлекается только после всех fallible/matching проверок.
        let candidate = self
            .candidate
            .take()
            .expect("candidate was validated above");
        // Token удерживает exclusive borrow slot-а до обязательного immediate commit-а.
        Ok(PreparedPostInstalledVideoPipelineCommit {
            owner_slot: self,
            candidate: Some(candidate),
        })
    }

    /// Забирает один lossless terminal outcome exactly once.
    pub(crate) fn drain_terminal_outcome(
        &mut self,
    ) -> Option<StagedVideoPipelineCandidateTerminalOutcome> {
        // `take` оставляет terminal slot пустым для следующей bounded transaction.
        self.terminal_outcome.take()
    }

    /// Возвращает snapshot bounded accounting counters.
    #[must_use]
    pub(crate) const fn diagnostics(&self) -> StagedVideoPipelineCandidateDiagnostics {
        // Snapshot не раскрывает materializer/backend owners.
        self.diagnostics
    }

    /// Возвращает текущий candidate descriptor для matching diagnostics/tests.
    #[must_use]
    pub(crate) fn candidate_descriptor(&self) -> Option<CandidateVideoPipelineDescriptor> {
        // Copy descriptor не позволяет мутировать candidate state.
        self.candidate
            .as_ref()
            .map(|candidate| candidate.descriptor)
    }

    /// Возвращает true только для единственного occupied candidate slot-а.
    #[must_use]
    pub(crate) const fn has_candidate(&self) -> bool {
        // Никакого hidden Vec/backend pool внутри owner-а нет.
        self.candidate.is_some()
    }

    /// Завершает local cleanup и lossless cancellation publication.
    fn finish_cancelled(&mut self, cause: DetachedVideoBackendCandidateCancellationCause) {
        // Exact request ID читается до owned candidate drop.
        let request_id = self
            .candidate
            .as_ref()
            .expect("finish_cancelled requires an admitted candidate")
            .request_id;
        // `take` освобождает materializer и submission binding ровно один раз.
        let _candidate = self
            .candidate
            .take()
            .expect("finish_cancelled requires an admitted candidate");
        // Counter изменяется только после фактического ownership take.
        self.diagnostics.cancellations = self.diagnostics.cancellations.saturating_add(1);
        // Terminal outcome нельзя overwrite/coalesce.
        self.publish_terminal(StagedVideoPipelineCandidateTerminalOutcome::Cancelled {
            request_id,
            cause,
        });
    }

    /// Публикует terminal outcome в заведомо пустой bounded slot.
    fn publish_terminal(&mut self, outcome: StagedVideoPipelineCandidateTerminalOutcome) {
        // Occupied terminal при admitted candidate является owner invariant violation.
        assert!(
            self.terminal_outcome.is_none(),
            "candidate terminal outcome slot must be empty before publication"
        );
        // Единственная assignment делает outcome lossless до explicit drain.
        self.terminal_outcome = Some(outcome);
    }
}

impl<Materializer, SubmissionBinding> Default
    for StagedVideoPipelineCandidateSlot<Materializer, SubmissionBinding>
{
    fn default() -> Self {
        // Default не создаёт resources и эквивалентен explicit empty slot.
        Self::new()
    }
}

/// App-side active pointers без player/media ownership.
pub(crate) struct ActiveVideoPipelinePointers<Materializer, SubmissionBinding> {
    /// Active backend class остаётся matching materializer-у.
    backend_kind: VideoBackendKind,

    /// Active renderer materializer pointer.
    materializer: Materializer,

    /// Active submitted-release queue binding.
    submission_binding: SubmissionBinding,
}

impl<Materializer, SubmissionBinding> ActiveVideoPipelinePointers<Materializer, SubmissionBinding> {
    /// Создаёт active pointer fixture/adapter без backend startup.
    #[must_use]
    pub(crate) const fn new(
        backend_kind: VideoBackendKind,
        materializer: Materializer,
        submission_binding: SubmissionBinding,
    ) -> Self {
        // Constructor только группирует уже существующие owners.
        Self {
            backend_kind,
            materializer,
            submission_binding,
        }
    }

    /// Возвращает active backend class без доступа к storage fields.
    #[must_use]
    pub(crate) const fn backend_kind(&self) -> VideoBackendKind {
        // Named accessor сохраняет boundary intent.
        self.backend_kind
    }

    /// Возвращает materializer reference только для owner-level adapter/tests.
    #[must_use]
    pub(crate) const fn materializer(&self) -> &Materializer {
        // Borrow не меняет active ownership.
        &self.materializer
    }

    /// Возвращает submission binding reference без release/rebind side effects.
    #[must_use]
    pub(crate) const fn submission_binding(&self) -> &SubmissionBinding {
        // Borrow не запускает queue wait или callback drain.
        &self.submission_binding
    }
}

/// Validated post-Installed token: все fallible work завершено до его создания.
#[must_use = "после matching Installed token обязан немедленно выполнить infallible commit"]
pub(crate) struct PreparedPostInstalledVideoPipelineCommit<'slot, Materializer, SubmissionBinding> {
    /// Exclusive borrow не позволяет lifecycle owner-у cancel/drop slot до commit-а.
    owner_slot: &'slot mut StagedVideoPipelineCandidateSlot<Materializer, SubmissionBinding>,

    /// `Some` содержит заранее подготовленные pointers; allocation/startup здесь нет.
    candidate: Option<StagedVideoPipelineCandidate<Materializer, SubmissionBinding>>,
}

impl<Materializer, SubmissionBinding>
    PreparedPostInstalledVideoPipelineCommit<'_, Materializer, SubmissionBinding>
{
    /// Infallibly заменяет только app-side pointers/binding после player Installed.
    pub(crate) fn commit(
        mut self,
        active: &mut ActiveVideoPipelinePointers<Materializer, SubmissionBinding>,
    ) {
        // Candidate уже прошёл request/generation/configuration validation.
        let candidate = self
            .candidate
            .take()
            .expect("prepared post-Installed commit owns candidate pointers");
        // IDs копируются до pointer move для lossless terminal outcome.
        let request_id = candidate.request_id;
        // Exact renderer generation фиксируется в commit diagnostics.
        let renderer_generation = candidate.renderer_generation;
        // Новый active aggregate строится только из заранее созданных owners.
        let replacement = ActiveVideoPipelinePointers {
            backend_kind: candidate.descriptor.backend_kind(),
            materializer: candidate.materializer,
            submission_binding: candidate.submission_binding,
        };

        // Единственная active mutation — pointer/binding replacement без Result/callback/wait.
        *active = replacement;
        // Commit counter изменяется в том же owner turn-е.
        self.owner_slot.diagnostics.commits = self.owner_slot.diagnostics.commits.saturating_add(1);
        // Installed terminal публикуется до возврата commit primitive-а.
        self.owner_slot
            .publish_terminal(StagedVideoPipelineCandidateTerminalOutcome::Installed {
                request_id,
                renderer_generation,
            });
    }
}

impl<Materializer, SubmissionBinding> Drop
    for PreparedPostInstalledVideoPipelineCommit<'_, Materializer, SubmissionBinding>
{
    fn drop(&mut self) {
        // Успешный commit уже забрал candidate и ничего восстанавливать не должен.
        let Some(candidate) = self.candidate.take() else {
            return;
        };
        // Token является единственным owner-ом после slot take.
        assert!(
            self.owner_slot.candidate.is_none(),
            "post-Installed commit token cannot overwrite another candidate"
        );
        // Abandoned token возвращает pointers в commit-required state, а не drop-ит их.
        self.owner_slot.candidate = Some(candidate);
    }
}

#[cfg(test)]
mod tests;
