//! Session-owned lifecycle worker-receipted demux seek-а.

use std::time::Instant;

use media_core::{DemuxSeekRequest, DemuxSeekResult, MediaTime};

use crate::seek_state::{SeekCommitState, SeekTargetRetention};
use crate::{
    MediaInstanceId, PlaybackResumeIntent, PlayerError, PlayerErrorKind,
    PreparedDemuxSeekLandingPolicy, PreparedDemuxSeekMode, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId, SeekMode,
};

use super::PlayerSession;

/// Player-owned параметры authoritative demux receipt без позиционного списка аргументов.
#[derive(Debug, Clone, Copy)]
pub(super) struct AcceptedDemuxSeekIntent {
    generation: u64,
    seek_mode: SeekMode,
    target_position: MediaTime,
    landing_policy: PreparedDemuxSeekLandingPolicy,
    resume_intent: PlaybackResumeIntent,
    target_retention: SeekTargetRetention,
    public_accepted_at: Instant,
}

impl AcceptedDemuxSeekIntent {
    /// Собирает immutable intent, общий для synchronous и worker-receipted routes.
    pub(super) const fn new(
        generation: u64,
        seek_mode: SeekMode,
        target_position: MediaTime,
        landing_policy: PreparedDemuxSeekLandingPolicy,
        resume_intent: PlaybackResumeIntent,
        target_retention: SeekTargetRetention,
        public_accepted_at: Instant,
    ) -> Self {
        Self {
            generation,
            seek_mode,
            target_position,
            landing_policy,
            resume_intent,
            target_retention,
            public_accepted_at,
        }
    }

    /// Возвращает generation для diagnostics и trace fence до создания commit-а.
    pub(super) const fn generation(self) -> u64 {
        self.generation
    }

    /// Возвращает public target для diagnostics до создания commit-а.
    pub(super) const fn target_position(self) -> MediaTime {
        self.target_position
    }

    /// Создаёт commit с отдельным authoritative receipt/timeout origin.
    pub(super) fn into_seek_commit(
        self,
        actual_position: MediaTime,
        receipt_accepted_at: Instant,
    ) -> SeekCommitState {
        SeekCommitState {
            generation: self.generation,
            seek_mode: self.seek_mode,
            target_position: self.target_position,
            actual_position,
            landing_policy: self.landing_policy,
            started_at: receipt_accepted_at,
            public_accepted_at: self.public_accepted_at,
            resume_intent: self.resume_intent,
            target_retention: self.target_retention,
        }
    }
}

/// Player-owned settlement prepared seek failure-а после уже начатой transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PreparedDemuxSeekFailureDisposition {
    /// Обычная ошибка seek-а откатывает transition в recoverable `Paused`.
    RecoverableRollback,
    /// Seek был запущен из `Failed`: exact causal fatal error должен восстановиться.
    PreserveCausalFailure(PlayerError),
    /// Defensive fallback сохраняет `Failed`, если нарушен invariant наличия causal error.
    PreserveFailedState,
}

impl PreparedDemuxSeekFailureDisposition {
    /// Захватывает causal state до временного перехода session в `Seeking`.
    pub(super) fn capture(session: &PlayerSession) -> Self {
        match (
            session.playback_state(),
            session.snapshot().last_error.as_ref(),
        ) {
            (crate::PlaybackState::Failed, Some(causal_error)) => {
                Self::PreserveCausalFailure(causal_error.clone())
            }
            (crate::PlaybackState::Failed, None) => Self::PreserveFailedState,
            (_, _) => Self::RecoverableRollback,
        }
    }
}

/// Явный installed prepared-seek route без позиционного boolean-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreparedDemuxSeekEnqueueRoute {
    /// Media использует обычный synchronous demux seek.
    Synchronous,
    /// Worker принял request и обещал terminal receipt.
    WorkerAccepted,
}

/// Enqueue failure сохраняет policy, захваченную до начала seek transition.
#[derive(Debug, Clone)]
pub(super) struct PreparedDemuxSeekEnqueueFailure {
    /// Typed player error для recoverable rollback или secondary diagnostics.
    error: PlayerError,
    /// Способ завершить уже начатую transition.
    disposition: PreparedDemuxSeekFailureDisposition,
}

/// Player-owned semantic intent, который должен пережить worker round-trip без потерь.
#[derive(Debug, Clone)]
pub(super) struct PreparedDemuxSeekIntent {
    /// Installed instance fence на момент enqueue.
    pub(super) media_instance_id: Option<MediaInstanceId>,
    /// Уже начатая pipeline seek generation.
    pub(super) generation: u64,
    /// Public relative target transaction-а.
    pub(super) target_position: MediaTime,
    /// User intent точности/скорости.
    pub(super) seek_mode: SeekMode,
    /// Stable play/pause intent после final commit.
    pub(super) resume_intent: PlaybackResumeIntent,
    /// Range owner, который имеет право инвалидировать target во время refresh-а.
    pub(super) target_retention: SeekTargetRetention,
    /// Monotonic public-command origin, который нельзя сдвигать к worker receipt.
    pub(super) public_accepted_at: Instant,
    /// Failure policy, захваченная до временного `Seeking` state.
    pub(super) failure_disposition: PreparedDemuxSeekFailureDisposition,
}

/// Correlation state request-а, который ещё не вернул authoritative demux anchor.
#[derive(Debug, Clone)]
pub(super) struct PendingPreparedDemuxSeek {
    /// Exact player-owned identity.
    request_id: PreparedDemuxSeekRequestId,
    /// Monotonic enqueue origin для worker round-trip diagnostics.
    enqueued_at: Instant,
    /// Installed instance fence на момент enqueue.
    media_instance_id: Option<MediaInstanceId>,
    /// Уже начатая pipeline seek generation.
    generation: u64,
    /// Public relative target transaction-а.
    target_position: MediaTime,
    /// Exact absolute demux target, который receipt обязан подтвердить.
    requested_demux_position: MediaTime,
    /// User intent точности/скорости.
    seek_mode: SeekMode,
    /// Source-owned contract выбора final playback position.
    landing_policy: PreparedDemuxSeekLandingPolicy,
    /// Stable play/pause intent после final commit.
    resume_intent: PlaybackResumeIntent,
    /// Range owner, который сохраняется через asynchronous worker receipt.
    target_retention: SeekTargetRetention,
    /// Monotonic public-command origin для seek-to-presentation acceptance.
    public_accepted_at: Instant,
    /// Failure policy, захваченная до временного `Seeking` state.
    failure_disposition: PreparedDemuxSeekFailureDisposition,
}

impl PendingPreparedDemuxSeek {
    /// Разрешает HLS opt-in только когда receipt действительно доказал post-target actual.
    fn landing_policy_for_result(&self, result: DemuxSeekResult) -> PreparedDemuxSeekLandingPolicy {
        if self.landing_policy == PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget
            && result.actual_position >= self.requested_demux_position
        {
            return PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget;
        }
        PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget
    }
}

/// Runtime port, request allocator и единственный current pending intent.
#[derive(Debug)]
pub(super) struct PreparedDemuxSeekRuntime {
    /// Installed prepared-media execution mode.
    mode: PreparedDemuxSeekMode,
    /// Следующая монотонная request identity.
    next_request_id: u64,
    /// Только latest seek может войти в player commit.
    pending: Option<PendingPreparedDemuxSeek>,
}

impl Default for PreparedDemuxSeekRuntime {
    /// Legacy providers остаются synchronous.
    fn default() -> Self {
        Self {
            mode: PreparedDemuxSeekMode::Synchronous,
            next_request_id: 1,
            pending: None,
        }
    }
}

impl PreparedDemuxSeekRuntime {
    /// Создаёт detached runtime, который затем целиком переносится в installed session.
    pub(super) fn detached(mode: PreparedDemuxSeekMode) -> Self {
        Self {
            mode,
            next_request_id: 1,
            pending: None,
        }
    }

    /// Устанавливает уже использованный detached runtime без сброса allocator-а/port-а.
    pub(super) fn install_detached(&mut self, detached: Self) {
        *self = detached;
    }

    /// Устанавливает mode exact нового media и сбрасывает старые fences.
    pub(super) fn install(&mut self, mode: PreparedDemuxSeekMode) {
        self.mode = mode;
        self.next_request_id = 1;
        self.pending = None;
    }

    /// Возвращает runtime к legacy synchronous mode и дропает старый port.
    pub(super) fn reset(&mut self) {
        self.install(PreparedDemuxSeekMode::Synchronous);
    }

    /// Сообщает one-shot seek orchestration, что demux replacement готовится вне player-owner.
    ///
    /// Этот intent-method не раскрывает конкретный port вызывающему коду: session выбирает
    /// существующую асинхронную seek transaction, а request/receipt fences остаются здесь.
    pub(super) const fn routes_one_shot_seek_through_worker(&self) -> bool {
        matches!(self.mode, PreparedDemuxSeekMode::WorkerReceipted { .. })
    }

    /// Не позволяет demux loop читать post-seek events до authoritative receipt-а.
    pub(super) const fn receipt_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Возвращает semantic target latest worker request-а без раскрытия port state.
    pub(super) fn pending_timeline_target(&self) -> Option<(MediaTime, SeekTargetRetention)> {
        self.pending
            .as_ref()
            .map(|pending| (pending.target_position, pending.target_retention))
    }

    /// Старый intent больше не может войти в commit после нового timeline command-а.
    pub(super) fn supersede_pending(&mut self) {
        self.pending = None;
    }

    /// Передаёт seek port-у и публикует pending только после успешного enqueue.
    pub(super) fn enqueue(
        &mut self,
        request: DemuxSeekRequest,
        intent: PreparedDemuxSeekIntent,
    ) -> Result<PreparedDemuxSeekEnqueueRoute, PreparedDemuxSeekEnqueueFailure> {
        let PreparedDemuxSeekMode::WorkerReceipted {
            port,
            landing_policy,
        } = &self.mode
        else {
            return Ok(PreparedDemuxSeekEnqueueRoute::Synchronous);
        };
        let failure_disposition = intent.failure_disposition.clone();
        let request_id = PreparedDemuxSeekRequestId::new(self.next_request_id);
        let enqueued_at = Instant::now();
        let next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or_else(|| PreparedDemuxSeekEnqueueFailure {
                    error: PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        "demux seek request identity space exhausted",
                    ),
                    disposition: failure_disposition.clone(),
                })?;
        let requested_demux_position = MediaTime::from_duration(request.timestamp);
        port.enqueue_seek(request_id, request).map_err(|error| {
            PreparedDemuxSeekEnqueueFailure {
                error: PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    format!("Не удалось передать seek demux worker-у: {error}"),
                ),
                disposition: failure_disposition.clone(),
            }
        })?;
        self.next_request_id = next_request_id;
        self.pending = Some(PendingPreparedDemuxSeek {
            request_id,
            enqueued_at,
            public_accepted_at: intent.public_accepted_at,
            media_instance_id: intent.media_instance_id,
            generation: intent.generation,
            target_position: intent.target_position,
            requested_demux_position,
            seek_mode: intent.seek_mode,
            landing_policy: *landing_policy,
            resume_intent: intent.resume_intent,
            target_retention: intent.target_retention,
            failure_disposition,
        });
        tracing::info!(
            request_id = ?request_id,
            generation = intent.generation,
            target_milliseconds = intent.target_position.as_duration().as_secs_f64() * 1_000.0,
            demux_target_milliseconds = request.timestamp.as_secs_f64() * 1_000.0,
            public_to_enqueue_ms = enqueued_at
                .saturating_duration_since(intent.public_accepted_at)
                .as_millis(),
            "Prepared demux seek request enqueued"
        );
        Ok(PreparedDemuxSeekEnqueueRoute::WorkerAccepted)
    }

    /// Передаёт detached candidate seek worker-у до создания installed generation.
    pub(super) fn enqueue_detached(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Result<Option<PreparedDemuxSeekRequestId>, PlayerError> {
        let PreparedDemuxSeekMode::WorkerReceipted { port, .. } = &self.mode else {
            return Ok(None);
        };
        let request_id = PreparedDemuxSeekRequestId::new(self.next_request_id);
        let next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "demux seek request identity space exhausted",
            )
        })?;
        port.enqueue_seek(request_id, request).map_err(|error| {
            PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Не удалось передать staged seek demux worker-у: {error}"),
            )
        })?;
        self.next_request_id = next_request_id;
        Ok(Some(request_id))
    }

    #[cfg(test)]
    pub(super) const fn next_request_id_for_tests(&self) -> u64 {
        self.next_request_id
    }

    /// Подменяет только test monotonic origin, не затрагивая receipt timeout clock.
    #[cfg(test)]
    pub(super) fn set_pending_public_accepted_at_for_tests(&mut self, public_accepted_at: Instant) {
        self.pending
            .as_mut()
            .expect("test worker request должен оставаться pending")
            .public_accepted_at = public_accepted_at;
    }

    /// Забирает все готовые receipts; session применит только exact latest.
    pub(super) fn poll_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        let PreparedDemuxSeekMode::WorkerReceipted { port, .. } = &self.mode else {
            return None;
        };
        port.poll_seek_receipt()
    }

    /// Забирает pending только при exact request identity.
    pub(super) fn take_matching_pending(
        &mut self,
        request_id: PreparedDemuxSeekRequestId,
    ) -> Option<PendingPreparedDemuxSeek> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            return self.pending.take();
        }
        None
    }
}

impl PlayerSession {
    /// Poll не блокирует player-owner и drain-ит stale/superseded receipts ради bound accounting.
    pub(super) fn service_prepared_demux_seek_receipts(&mut self) {
        while let Some(receipt) = self.prepared_demux_seek.poll_receipt() {
            let Some(pending) = self
                .prepared_demux_seek
                .take_matching_pending(receipt.request_id)
            else {
                continue;
            };
            if pending.media_instance_id != self.snapshot.media_instance_id
                || pending.generation != self.pipeline.seek_generation()
            {
                continue;
            }
            match receipt.outcome {
                PreparedDemuxSeekOutcome::Succeeded(result) => {
                    if result.requested_position == pending.requested_demux_position {
                        tracing::info!(
                            request_id = ?pending.request_id,
                            generation = pending.generation,
                            target_milliseconds =
                                pending.target_position.as_duration().as_secs_f64() * 1_000.0,
                            actual_milliseconds =
                                result.actual_position.as_duration().as_secs_f64() * 1_000.0,
                            elapsed_milliseconds = pending.enqueued_at.elapsed().as_millis(),
                            public_to_receipt_ms = pending
                                .public_accepted_at
                                .elapsed()
                                .as_millis(),
                            "Prepared demux seek receipt accepted"
                        );
                        self.accept_worker_receipted_demux_seek(pending, result);
                    } else {
                        self.settle_prepared_demux_seek_failure(
                            PlayerError::new(
                                PlayerErrorKind::SeekUnavailable,
                                "Demux worker вернул receipt для другого requested target",
                            ),
                            pending.failure_disposition,
                        );
                    }
                }
                PreparedDemuxSeekOutcome::Failed => {
                    self.settle_prepared_demux_seek_failure(
                        PlayerError::new(
                            PlayerErrorKind::SeekUnavailable,
                            "Demux worker не смог выполнить seek",
                        ),
                        pending.failure_disposition,
                    );
                }
                PreparedDemuxSeekOutcome::Cancelled => {
                    self.settle_prepared_demux_seek_failure(
                        PlayerError::new(
                            PlayerErrorKind::SeekUnavailable,
                            "Demux seek отменён media lifecycle",
                        ),
                        pending.failure_disposition,
                    );
                }
                PreparedDemuxSeekOutcome::Superseded | PreparedDemuxSeekOutcome::Stale => {
                    self.settle_prepared_demux_seek_failure(
                        PlayerError::new(
                            PlayerErrorKind::SeekUnavailable,
                            "Demux seek receipt устарел",
                        ),
                        pending.failure_disposition,
                    );
                }
            }
        }
    }

    /// Завершает rejected prepared enqueue согласно state до начала `Seeking`.
    pub(super) fn settle_prepared_demux_seek_enqueue_failure(
        &mut self,
        failure: PreparedDemuxSeekEnqueueFailure,
    ) {
        self.settle_prepared_demux_seek_failure(failure.error, failure.disposition);
    }

    /// Не позволяет secondary worker failure затереть уже опубликованный causal fatal error.
    fn settle_prepared_demux_seek_failure(
        &mut self,
        error: PlayerError,
        disposition: PreparedDemuxSeekFailureDisposition,
    ) {
        match disposition {
            PreparedDemuxSeekFailureDisposition::RecoverableRollback => {
                self.fail_started_demux_seek(error);
            }
            PreparedDemuxSeekFailureDisposition::PreserveCausalFailure(causal_error) => {
                self.clear_prepared_demux_seek_transition_after_failure();
                self.snapshot.last_error = Some(causal_error);
                self.set_playback_state(crate::PlaybackState::Failed);
                tracing::warn!(
                    secondary_error = %error,
                    causal_error = ?self.snapshot.last_error,
                    "Prepared demux seek failure не заменил causal fatal player state"
                );
            }
            PreparedDemuxSeekFailureDisposition::PreserveFailedState => {
                self.clear_prepared_demux_seek_transition_after_failure();
                self.set_playback_state(crate::PlaybackState::Failed);
                tracing::warn!(
                    secondary_error = %error,
                    "Prepared demux seek failure сохранил Failed state без causal error"
                );
            }
        }
    }

    /// Очищает только seek-local state, не меняя causal playback error/state.
    fn clear_prepared_demux_seek_transition_after_failure(&mut self) {
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.target_position = None;
    }

    /// Authoritative worker result входит в тот же existing final seek lifecycle.
    fn accept_worker_receipted_demux_seek(
        &mut self,
        pending: PendingPreparedDemuxSeek,
        result: DemuxSeekResult,
    ) {
        let landing_policy = pending.landing_policy_for_result(result);
        self.accept_demux_seek_result(
            AcceptedDemuxSeekIntent::new(
                pending.generation,
                pending.seek_mode,
                pending.target_position,
                landing_policy,
                pending.resume_intent,
                pending.target_retention,
                pending.public_accepted_at,
            ),
            result,
        );
    }
}
