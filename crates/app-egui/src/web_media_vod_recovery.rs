//! Seamless gate и app-owned runtime attachment для VOD endpoint recovery.
//!
//! Physical providers публикуют только typed expiry. Этот модуль удерживает старый
//! demux/seek runtime в `TemporarilyUnavailable`, пока app готовит полный fresh
//! candidate. Он не выполняет yt-dlp extraction и не коммитит player state.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    MediaMetadata, TrackInfo,
};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekPort, PreparedDemuxSeekReceipt,
    PreparedDemuxSeekRequestId,
};
use web_media_transport_api::{EndpointExpiryObserver, EndpointExpirySignal, SourceGeneration};

/// Gate state принадлежит одной подготовленной candidate generation lineage.
#[derive(Debug)]
enum VodEndpointRecoveryGateState {
    /// Speculative probes ещё могут законно отклонять соседние endpoints.
    Unarmed,
    /// Ни один physical component ещё не сообщил expiry.
    Dormant,
    /// Первый signal сохранён; дополнительные A/V/resource сигналы coalesce-ятся.
    Pending {
        /// Первый signal задаёт generation fence и diagnostics.
        signal: EndpointExpirySignal,
        /// Claim не должен запускать вторую параллельную extraction attempt.
        claimed: bool,
    },
    /// App не смог подготовить replacement; старый terminal outcome снова разрешён.
    Failed,
}

/// Shared state не содержит locator, cookies, headers или player identity.
#[derive(Debug)]
struct VodEndpointRecoveryShared {
    /// Poison считается invariant failure и восстанавливается только для terminal propagation.
    state: Mutex<VodEndpointRecoveryGateState>,
}

/// Cloneable runtime attachment, передаваемый и observer-у, и Installed app state.
#[derive(Clone)]
pub(crate) struct VodEndpointRecoveryAttachment {
    /// Один Arc гарантирует candidate-level A/V coalescing.
    shared: Arc<VodEndpointRecoveryShared>,
}

impl std::fmt::Debug for VodEndpointRecoveryAttachment {
    /// Debug показывает только lifecycle state и не раскрывает media identities.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VodEndpointRecoveryAttachment")
            .field("pending", &self.is_recovery_pending())
            .finish_non_exhaustive()
    }
}

impl VodEndpointRecoveryAttachment {
    /// Создаёт unarmed gate: ошибки speculative catalog probes не являются active expiry.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(VodEndpointRecoveryShared {
                state: Mutex::new(VodEndpointRecoveryGateState::Unarmed),
            }),
        }
    }

    /// Возвращает type-erased observer для transport request-а.
    #[must_use]
    pub(crate) fn observer(&self) -> Arc<dyn EndpointExpiryObserver> {
        Arc::new(self.clone())
    }

    /// Разрешает expiry publication только после окончательного выбора playable candidate-а.
    pub(crate) fn arm_after_candidate_finalization(&self) {
        let mut state = self.lock_state();
        if matches!(*state, VodEndpointRecoveryGateState::Unarmed) {
            *state = VodEndpointRecoveryGateState::Dormant;
        }
    }

    /// Забирает первый unclaimed signal ровно одной app recovery attempt-ой.
    pub(crate) fn claim_pending_signal(&self) -> Option<EndpointExpirySignal> {
        let mut state = self.lock_state();
        let VodEndpointRecoveryGateState::Pending { signal, claimed } = &mut *state else {
            return None;
        };
        if *claimed {
            return None;
        }
        *claimed = true;
        Some(signal.clone())
    }

    /// Сообщает wrapper-ам, что старый runtime больше не должен публиковать events.
    #[must_use]
    pub(crate) fn is_recovery_pending(&self) -> bool {
        matches!(
            *self.lock_state(),
            VodEndpointRecoveryGateState::Pending { .. }
        )
    }

    /// Возвращает generation первого coalesced signal-а для exact Installed fence.
    #[must_use]
    pub(crate) fn pending_source_generation(&self) -> Option<SourceGeneration> {
        match &*self.lock_state() {
            VodEndpointRecoveryGateState::Pending { signal, .. } => {
                Some(signal.source_generation())
            }
            VodEndpointRecoveryGateState::Unarmed
            | VodEndpointRecoveryGateState::Dormant
            | VodEndpointRecoveryGateState::Failed => None,
        }
    }

    /// Снимает seamless hold только после terminal failure replacement preparation-а.
    pub(crate) fn mark_recovery_failed(&self) {
        *self.lock_state() = VodEndpointRecoveryGateState::Failed;
    }

    /// Оборачивает candidate demuxer до передачи ownership player-у.
    pub(crate) fn wrap_demuxer(&self, demuxer: Box<dyn Demuxer + Send>) -> Box<dyn Demuxer + Send> {
        Box::new(VodEndpointRecoveryDemuxer {
            inner: demuxer,
            gate: self.clone(),
            deferred_failure: None,
        })
    }

    /// Оборачивает optional receipted seek port тем же candidate-level gate-ом.
    pub(crate) fn wrap_seek_port(
        &self,
        seek_port: Option<Arc<dyn PreparedDemuxSeekPort>>,
    ) -> Option<Arc<dyn PreparedDemuxSeekPort>> {
        seek_port.map(|inner| {
            Arc::new(VodEndpointRecoverySeekPort {
                inner,
                gate: self.clone(),
                withheld_receipts: Mutex::new(VecDeque::new()),
            }) as Arc<dyn PreparedDemuxSeekPort>
        })
    }

    /// Восстанавливает poisoned lock только для fail-closed terminal propagation.
    fn lock_state(&self) -> MutexGuard<'_, VodEndpointRecoveryGateState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl EndpointExpiryObserver for VodEndpointRecoveryAttachment {
    /// Первый post-finalization signal открывает gate; speculative failures игнорируются.
    fn observe_endpoint_expiry(&self, signal: EndpointExpirySignal) {
        let mut state = self.lock_state();
        if matches!(*state, VodEndpointRecoveryGateState::Dormant) {
            *state = VodEndpointRecoveryGateState::Pending {
                signal,
                claimed: false,
            };
        }
    }
}

/// Demux wrapper запрещает publication старой generation после expiry observation.
struct VodEndpointRecoveryDemuxer {
    /// Concrete provider/container остаётся единственным владельцем parser state.
    inner: Box<dyn Demuxer + Send>,
    /// Candidate-level gate общий с app coordinator и seek port-ом.
    gate: VodEndpointRecoveryAttachment,
    /// Исходная ошибка сохраняется до terminal failure recovery attempt-а.
    deferred_failure: Option<anyhow::Error>,
}

impl VodEndpointRecoveryDemuxer {
    /// Возвращает neutral readiness hint без отдельной polling policy.
    fn temporarily_unavailable() -> DemuxReadEvent {
        let retry_hint = DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER)
            .expect("minimum demux retry hint обязан быть валиден");
        DemuxReadEvent::TemporarilyUnavailable(retry_hint)
    }
}

impl Demuxer for VodEndpointRecoveryDemuxer {
    /// Track snapshot остаётся authoritative snapshot-ом установленного demuxer-а.
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    /// Duration не меняется до атомарного replacement commit-а.
    fn duration(&self) -> Option<std::time::Duration> {
        self.inner.duration()
    }

    /// Metadata не смешивается между old/fresh extraction generations.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.inner.media_metadata()
    }

    /// Seekability старого runtime остаётся стабильной до replacement-а.
    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    /// После expiry ни packet, ни EOF, ни TracksChanged старой generation не публикуются.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.gate.is_recovery_pending() {
            return Ok(Self::temporarily_unavailable());
        }
        if let Some(failure) = self.deferred_failure.take() {
            return Err(failure);
        }
        let event = self.inner.next_event();
        if self.gate.is_recovery_pending() {
            if let Err(failure) = event {
                self.deferred_failure = Some(failure);
            }
            return Ok(Self::temporarily_unavailable());
        }
        event
    }

    /// Legacy seek делегируется parser-у; expiry на subsequent read откроет gate.
    fn seek(&mut self, timestamp: std::time::Duration) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    /// Named seek mode сохраняется без positional downgrade.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }
}

/// Receipted seek wrapper удерживает старый completion до replacement outcome-а.
struct VodEndpointRecoverySeekPort {
    /// Existing provider adapter остаётся request/receipt authority.
    inner: Arc<dyn PreparedDemuxSeekPort>,
    /// Тот же gate coalesce-ит seek-triggered и read-ahead expiry.
    gate: VodEndpointRecoveryAttachment,
    /// Старые receipts не теряются при terminal recovery failure.
    withheld_receipts: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
}

impl PreparedDemuxSeekPort for VodEndpointRecoverySeekPort {
    /// До expiry enqueue полностью сохраняет прежнюю provider semantics.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        if self.gate.is_recovery_pending() {
            return Err(PreparedDemuxSeekEnqueueError::ReceiptQueueFull);
        }
        self.inner.enqueue_seek(request_id, request)
    }

    /// Pending recovery не позволяет старому seek receipt коммитить позицию.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        if self.gate.is_recovery_pending() {
            if let Some(receipt) = self.inner.poll_seek_receipt() {
                self.withheld_receipts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(receipt);
            }
            return None;
        }
        self.withheld_receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .or_else(|| self.inner.poll_seek_receipt())
    }
}

#[cfg(test)]
mod tests {
    use media_core::{DemuxReadEvent, DemuxSeekResult, MediaTime, finite_packet_read_event};
    use web_media_core::{
        CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
        SourceIdentity,
    };
    use web_media_transport_api::{
        EndpointExpiryReason, EndpointExpiryResourceKind, MediaComponentIdentity,
        MediaComponentRole,
    };

    use super::*;

    /// Fake доказывает publication boundary целого demuxer-а, а не helper state.
    struct ScriptedDemuxer {
        tracks: Vec<TrackInfo>,
        events: VecDeque<anyhow::Result<DemuxReadEvent>>,
    }

    impl Demuxer for ScriptedDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<std::time::Duration> {
            Some(std::time::Duration::from_secs(60))
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            self.events
                .pop_front()
                .unwrap_or_else(|| Ok(finite_packet_read_event(None)))
        }

        fn seek(&mut self, timestamp: std::time::Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    fn expiry_signal(generation: u64) -> EndpointExpirySignal {
        EndpointExpirySignal::new(
            MediaComponentIdentity::new(
                CandidateIdentity::new(
                    SourceIdentity::new(41),
                    ExtractionGeneration::new(generation),
                    CandidateFormatIdentity::new("exact-fixture").expect("format identity"),
                ),
                SemanticIdentity::new(SourceIdentity::new(41), "semantic-fixture")
                    .expect("semantic identity"),
                MediaComponentRole::Muxed,
            )
            .expect("same source lineage"),
            SourceGeneration::new(generation),
            EndpointExpiryResourceKind::MediaSegment,
            EndpointExpiryReason::AuthorizationExpired,
        )
    }

    /// Первый expiry блокирует old packets, coalesce-ит A/V сигналы и claim-ится один раз.
    #[test]
    fn expiry_gate_holds_old_demux_publication_until_recovery_terminal() {
        let attachment = VodEndpointRecoveryAttachment::new();
        attachment.arm_after_candidate_finalization();
        let mut demuxer = attachment.wrap_demuxer(Box::new(ScriptedDemuxer {
            tracks: Vec::new(),
            events: VecDeque::from([Ok(DemuxReadEvent::EndOfStream)]),
        }));

        attachment.observe_endpoint_expiry(expiry_signal(7));
        attachment.observe_endpoint_expiry(expiry_signal(8));

        assert!(matches!(
            demuxer.next_event().expect("pending recovery readiness"),
            DemuxReadEvent::TemporarilyUnavailable(_)
        ));
        assert_eq!(
            attachment
                .claim_pending_signal()
                .expect("first signal")
                .source_generation(),
            SourceGeneration::new(7)
        );
        assert!(attachment.claim_pending_signal().is_none());

        attachment.mark_recovery_failed();
        assert!(matches!(
            demuxer.next_event().expect("old terminal resumes"),
            DemuxReadEvent::EndOfStream
        ));
    }

    /// Неактивный соседний probe может вернуть 404, не отравив выбранный playable runtime.
    #[test]
    fn speculative_expiry_before_candidate_finalization_is_ignored() {
        let attachment = VodEndpointRecoveryAttachment::new();

        attachment.observe_endpoint_expiry(expiry_signal(4));
        assert!(!attachment.is_recovery_pending());
        assert!(attachment.claim_pending_signal().is_none());

        attachment.arm_after_candidate_finalization();
        attachment.observe_endpoint_expiry(expiry_signal(5));
        assert_eq!(
            attachment
                .claim_pending_signal()
                .expect("post-finalization signal")
                .source_generation(),
            SourceGeneration::new(5)
        );
    }

    /// Generation видна только во время pending recovery.
    #[test]
    fn pending_generation_is_visible_only_while_gate_is_pending() {
        let attachment = VodEndpointRecoveryAttachment::new();
        attachment.arm_after_candidate_finalization();
        assert_eq!(attachment.pending_source_generation(), None);
        attachment.observe_endpoint_expiry(expiry_signal(9));
        assert_eq!(
            attachment.pending_source_generation(),
            Some(SourceGeneration::new(9))
        );
        attachment.mark_recovery_failed();
        assert_eq!(attachment.pending_source_generation(), None);
    }
}
