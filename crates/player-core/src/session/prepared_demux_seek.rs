//! Session-owned lifecycle worker-receipted demux seek-а.

use media_core::{DemuxSeekRequest, DemuxSeekResult, MediaTime};

use crate::{
    MediaInstanceId, PlaybackResumeIntent, PlayerError, PlayerErrorKind, PreparedDemuxSeekMode,
    PreparedDemuxSeekOutcome, PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId, SeekMode,
};

use super::PlayerSession;

/// Correlation state request-а, который ещё не вернул authoritative demux anchor.
#[derive(Debug, Clone, Copy)]
pub(super) struct PendingPreparedDemuxSeek {
    /// Exact player-owned identity.
    request_id: PreparedDemuxSeekRequestId,
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
    /// Stable play/pause intent после final commit.
    resume_intent: PlaybackResumeIntent,
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

    /// Не позволяет demux loop читать post-seek events до authoritative receipt-а.
    pub(super) const fn receipt_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Старый intent больше не может войти в commit после нового timeline command-а.
    pub(super) fn supersede_pending(&mut self) {
        self.pending = None;
    }

    /// Передаёт seek port-у и публикует pending только после успешного enqueue.
    pub(super) fn enqueue(
        &mut self,
        request: DemuxSeekRequest,
        media_instance_id: Option<MediaInstanceId>,
        generation: u64,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
    ) -> Result<bool, PlayerError> {
        let PreparedDemuxSeekMode::WorkerReceipted { port } = &self.mode else {
            return Ok(false);
        };
        let request_id = PreparedDemuxSeekRequestId::new(self.next_request_id);
        let next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "demux seek request identity space exhausted",
            )
        })?;
        let requested_demux_position = MediaTime::from_duration(request.timestamp);
        port.enqueue_seek(request_id, request).map_err(|error| {
            PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Не удалось передать seek demux worker-у: {error}"),
            )
        })?;
        self.next_request_id = next_request_id;
        self.pending = Some(PendingPreparedDemuxSeek {
            request_id,
            media_instance_id,
            generation,
            target_position,
            requested_demux_position,
            seek_mode,
            resume_intent,
        });
        Ok(true)
    }

    /// Передаёт detached candidate seek worker-у до создания installed generation.
    pub(super) fn enqueue_detached(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Result<Option<PreparedDemuxSeekRequestId>, PlayerError> {
        let PreparedDemuxSeekMode::WorkerReceipted { port } = &self.mode else {
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

    /// Забирает все готовые receipts; session применит только exact latest.
    pub(super) fn poll_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        let PreparedDemuxSeekMode::WorkerReceipted { port } = &self.mode else {
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
                        self.accept_worker_receipted_demux_seek(pending, result);
                    } else {
                        self.fail_started_demux_seek(PlayerError::new(
                            PlayerErrorKind::SeekUnavailable,
                            "Demux worker вернул receipt для другого requested target",
                        ));
                    }
                }
                PreparedDemuxSeekOutcome::Failed => {
                    self.fail_started_demux_seek(PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        "Demux worker не смог выполнить seek",
                    ));
                }
                PreparedDemuxSeekOutcome::Cancelled => {
                    self.fail_started_demux_seek(PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        "Demux seek отменён media lifecycle",
                    ));
                }
                PreparedDemuxSeekOutcome::Superseded | PreparedDemuxSeekOutcome::Stale => {
                    self.fail_started_demux_seek(PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        "Demux seek receipt устарел",
                    ));
                }
            }
        }
    }

    /// Authoritative worker result входит в тот же existing final seek lifecycle.
    fn accept_worker_receipted_demux_seek(
        &mut self,
        pending: PendingPreparedDemuxSeek,
        result: DemuxSeekResult,
    ) {
        self.accept_demux_seek_result(
            pending.generation,
            pending.seek_mode,
            pending.target_position,
            pending.resume_intent,
            result,
        );
    }
}
