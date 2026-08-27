//! Renderer-bound candidate video pipeline resources для strong media install.
//!
//! Session 00C создаёт bounded resource boundary, а Session 00C1 использует его
//! после player `Installed` только для exact infallible pointer commit-а.

#![allow(
    dead_code,
    reason = "Session 00C1 validates the boundary before later coordinator call-site migration"
)]

use std::sync::{Arc, Mutex};

use player_core::{
    MediaInstallRequestId, MediaInstallVideoBackendConstraint, MediaInstallVideoResourcePort,
    PlayerVideoDecoderThreadConfig,
};
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
    DetachedVideoBackendResourceError, DetachedVideoBackendResourcePort,
};

use crate::video_pipeline_selector::{VideoBackendKind, VideoPipelinePlan};

mod protocol;
mod resource_driver;

pub(crate) use protocol::{
    PostInstalledVideoPipelineInvariantViolation, RendererGeneration,
    StagedVideoPipelineCandidateCancelError, StagedVideoPipelineCandidateDiagnostics,
    StagedVideoPipelineCandidateMatchError, StagedVideoPipelineCandidateStatusError,
    StagedVideoPipelineCandidateTerminalOutcome,
};
use protocol::{StagedVideoPipelineCandidate, StagedVideoPipelineCandidateState};

#[allow(
    unused_imports,
    reason = "production callsite подключается в Session 10D"
)]
pub(crate) use resource_driver::WgpuCandidateVideoPipelineResourceDriver;
use resource_driver::{CandidateVideoPipelineDescriptor, CandidateVideoPipelineResourceDriver};

/// App-owned shared handle exact candidate slot-а; player видит только neutral port.
pub(crate) struct AppVideoPipelineCandidateOwner<Materializer, SubmissionBinding> {
    slot: Arc<Mutex<StagedVideoPipelineCandidateSlot<Materializer, SubmissionBinding>>>,
}

impl<Materializer, SubmissionBinding>
    AppVideoPipelineCandidateOwner<Materializer, SubmissionBinding>
{
    /// После exact `Installed` infallibly переключает только заранее подготовленные app pointers.
    pub(crate) fn commit_installed(
        &self,
        request_id: MediaInstallRequestId,
        renderer_generation: RendererGeneration,
        active: &mut ActiveVideoPipelinePointers<Materializer, SubmissionBinding>,
    ) -> Result<(), PostInstalledVideoPipelineInvariantViolation> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = slot.prepare_post_installed_commit(request_id, renderer_generation)?;
        token.commit(active);
        Ok(())
    }

    /// Неблокирующе забирает app-half terminal outcome exactly once.
    pub(crate) fn drain_terminal_outcome(
        &self,
    ) -> Option<StagedVideoPipelineCandidateTerminalOutcome> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain_terminal_outcome()
    }

    /// Показывает, удерживает ли owner ровно один staged candidate.
    pub(crate) fn has_candidate(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_candidate()
    }
}

/// Создаёт paired production boundary: app удерживает slot, player получает только port.
pub(crate) fn player_selected_video_candidate_boundary<Driver>(
    renderer_generation: RendererGeneration,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    backend_constraint: MediaInstallVideoBackendConstraint,
    driver: Driver,
) -> (
    AppVideoPipelineCandidateOwner<Driver::Materializer, Driver::SubmissionBinding>,
    MediaInstallVideoResourcePort,
)
where
    Driver: CandidateVideoPipelineResourceDriver + Send + 'static,
    Driver::Materializer: Send + 'static,
    Driver::SubmissionBinding: Send + 'static,
{
    let slot = Arc::new(Mutex::new(StagedVideoPipelineCandidateSlot::new()));
    let owner = AppVideoPipelineCandidateOwner {
        slot: Arc::clone(&slot),
    };
    let port = PlayerSelectedVideoCandidatePort {
        slot,
        renderer_generation,
        decoder_thread_config,
        driver,
    };
    (
        owner,
        MediaInstallVideoResourcePort::new(backend_constraint, port),
    )
}

/// Player-side adapter concrete app resource owner-а.
struct PlayerSelectedVideoCandidatePort<Driver>
where
    Driver: CandidateVideoPipelineResourceDriver,
{
    slot: Arc<
        Mutex<StagedVideoPipelineCandidateSlot<Driver::Materializer, Driver::SubmissionBinding>>,
    >,
    renderer_generation: RendererGeneration,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    driver: Driver,
}

impl<Driver> DetachedVideoBackendResourcePort for PlayerSelectedVideoCandidatePort<Driver>
where
    Driver: CandidateVideoPipelineResourceDriver + Send,
    Driver::Materializer: Send,
    Driver::SubmissionBinding: Send,
{
    type RequestId = MediaInstallRequestId;

    fn request_detached_backend(
        &mut self,
        request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        let (request_id, selection) = request.into_parts();
        let plan = match VideoPipelinePlan::from_player_selection(
            &selection,
            self.decoder_thread_config,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(DetachedVideoBackendReply::unavailable(
                    request_id,
                    DetachedVideoBackendResourceError::Unavailable {
                        reason: error.to_string(),
                    },
                ));
            }
        };
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| DetachedVideoBackendPortError)?;
        Ok(slot.prepare_and_stage(request_id, self.renderer_generation, plan, &mut self.driver))
    }

    fn publish_candidate_status(
        &mut self,
        status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| DetachedVideoBackendPortError)?;
        let mut player_owner_abort = PlayerOwnerAbortAcknowledgement;
        slot.record_player_status(status, self.renderer_generation, &mut player_owner_abort)
            .map_err(|_| DetachedVideoBackendPortError)
    }

    fn cancel_candidate(
        &mut self,
        request_id: Self::RequestId,
        cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| DetachedVideoBackendPortError)?;
        let Some(candidate) = slot.candidate.as_ref() else {
            return Err(DetachedVideoBackendPortError);
        };
        if candidate.request_id != request_id {
            return Err(DetachedVideoBackendPortError);
        }
        slot.finish_cancelled(cause);
        Ok(())
    }
}

/// В production status callback уже выполняется player owner-ом: ошибка вернётся ему напрямую.
struct PlayerOwnerAbortAcknowledgement;

impl DetachedVideoBackendResourcePort for PlayerOwnerAbortAcknowledgement {
    type RequestId = MediaInstallRequestId;

    fn request_detached_backend(
        &mut self,
        _request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        Err(DetachedVideoBackendPortError)
    }

    fn publish_candidate_status(
        &mut self,
        _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Err(DetachedVideoBackendPortError)
    }

    fn cancel_candidate(
        &mut self,
        _request_id: Self::RequestId,
        _cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }
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

    /// Возвращает ownership aggregate app owner-у после infallible pointer commit-а.
    pub(crate) fn into_parts(self) -> (VideoBackendKind, Materializer, SubmissionBinding) {
        (
            self.backend_kind,
            self.materializer,
            self.submission_binding,
        )
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
