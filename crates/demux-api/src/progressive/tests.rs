use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::task::{Context, Wake, Waker};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use media_core::{
    DemuxActiveReadInterrupter, DemuxActiveReadInterruptionCapability,
    DemuxActiveReadInterruptionPort, DemuxActiveReadInterruptionReason,
    DemuxActiveReadInterruptionResult, DemuxReadEvent, DemuxRetryHint,
    DemuxSeekCancellationCompletion, DemuxSeekCancellationToken, DemuxSeekRequest, DemuxSeekResult,
    DemuxSeekability, DemuxTrackListUpdate, Demuxer, MediaDemuxError, MediaTime, Packet,
    TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind,
};
use source_core::CancellationToken;

use super::worker::{
    ProgressiveMessage, ProgressivePushOutcome, ProgressiveSeekCommand, ProgressiveSharedState,
    push_progressive_message, wait_for_seek_command,
};
use super::{
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveAsyncSeekReceipt, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxPacketTooLargeError, ProgressiveDemuxReadiness, ProgressiveDemuxReadinessPort,
    ProgressiveDemuxStartupError, ProgressiveDemuxer, ProgressivePreviewWorkerResultPolicy,
    ProgressiveRuntimeGeneration, ProgressiveSeekController, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};

mod preview_cancellation;
mod readiness;
mod receipted_seek;
mod stale_seek;
mod sync_preview_receipt;
mod worker_lifecycle;

/// Blocking fake сохраняет главный production invariant: inner read может ждать сколько угодно.
struct BlockingChannelDemuxer {
    /// Test owner публикует готовые exact demux events.
    receiver: Receiver<DemuxReadEvent>,
}

/// Уникальные test waker-ы насыщают bounded cancellation registry без executor-а.
struct ReadinessCountingWake {
    /// Wake side effect не даёт clippy спутать unique registry fixture с noop waker-ом.
    wake_count: AtomicUsize,
}

impl Wake for ReadinessCountingWake {
    /// Saturation test наблюдает fail-closed token state, а не отдельные wake callbacks.
    fn wake(self: Arc<Self>) {
        self.wake_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// Gated seekable fake делает каждый inner read двухфазным и наблюдаемым.
struct GatedSeekableEventDemuxer {
    /// Rendezvous сообщает номер read до ожидания управляемого event-а.
    read_started: SyncSender<usize>,
    /// Test owner освобождает ровно один уже начатый read.
    event_receiver: Receiver<DemuxReadEvent>,
    /// Следующий монотонный номер read начинается с единицы.
    next_read_sequence: usize,
    /// Seek result сохраняет exact requested position.
    position: Duration,
}

/// Наблюдаемые фазы interruptible body read и следующего replacement seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptibleReadLifecycleEvent {
    /// Worker вошёл в старый physical body read.
    ReadStarted,
    /// Старый body owner был dropped после interruption signal-а.
    BodyDropped,
    /// Worker начал transactional replacement только после unwind старого read-а.
    ReplacementStarted,
}

/// Drop guard моделирует владение pending HTTP body future/resource-ом.
struct InterruptibleBodyDropGuard {
    /// Один ordered channel делает относительный порядок фаз детерминированным.
    lifecycle_sender: SyncSender<InterruptibleReadLifecycleEvent>,
}

impl Drop for InterruptibleBodyDropGuard {
    /// Фиксирует физическое освобождение старого body owner-а до replacement boundary.
    fn drop(&mut self) {
        let _ = self
            .lifecycle_sender
            .send(InterruptibleReadLifecycleEvent::BodyDropped);
    }
}

/// Shared controller сигналит только active read-у и никогда не ждёт worker/queue.
struct TestActiveReadInterrupter {
    /// Истина только пока fake физически ждёт old body.
    read_is_active: AtomicBool,
    /// Число accepted current-runtime requests, пославших interruption signal.
    request_count: AtomicUsize,
    /// Capacity-one sender используется только через `try_send`, без ожидания receiver-а.
    interruption_sender: SyncSender<()>,
}

impl DemuxActiveReadInterrupter for TestActiveReadInterrupter {
    /// Ставит один nonblocking сигнал либо сообщает quiescent state.
    fn request_active_read_interruption(
        &self,
        reason: DemuxActiveReadInterruptionReason,
    ) -> DemuxActiveReadInterruptionResult {
        assert_eq!(
            reason,
            DemuxActiveReadInterruptionReason::ReceiptedSeekEnqueued
        );
        self.request_count.fetch_add(1, Ordering::SeqCst);
        if self.read_is_active.swap(false, Ordering::SeqCst) {
            self.interruption_sender
                .try_send(())
                .expect("active test read обязан владеть свободным signal slot-ом");
            DemuxActiveReadInterruptionResult::InterruptionRequestedRestartable
        } else {
            DemuxActiveReadInterruptionResult::AlreadyQuiescent
        }
    }
}

/// Seekable fake моделирует stalled old body, whole-parser unwind и transactional rollback.
struct InterruptibleReceiptedSeekDemuxer {
    /// Stable controller возвращается через generic demux capability.
    interruption_controller: Arc<TestActiveReadInterrupter>,
    /// Opaque cloneable port не раскрывает fake fields progressive owner-у.
    interruption_port: DemuxActiveReadInterruptionPort,
    /// Первый body read ждёт interruption signal-а.
    interruption_receiver: Receiver<()>,
    /// Ordered lifecycle events доказывают Drop-before-replacement.
    lifecycle_sender: SyncSender<InterruptibleReadLifecycleEvent>,
    /// Scripted replacement может fail-closed проверить rollback.
    replacement_fails: bool,
    /// Только самый первый read моделирует obsolete body.
    first_read: bool,
    /// Текущая committed timeline position старого либо нового source-а.
    position: Duration,
    /// После restart/commit fake публикует ровно один packet.
    packet_emitted: bool,
}

impl Demuxer for InterruptibleReceiptedSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn active_read_interruption(&self) -> DemuxActiveReadInterruptionCapability {
        DemuxActiveReadInterruptionCapability::Supported(self.interruption_port.clone())
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.first_read {
            self.first_read = false;
            self.interruption_controller
                .read_is_active
                .store(true, Ordering::SeqCst);
            self.lifecycle_sender
                .send(InterruptibleReadLifecycleEvent::ReadStarted)
                .expect("test lifecycle receiver должен жить");
            let _body_guard = InterruptibleBodyDropGuard {
                lifecycle_sender: self.lifecycle_sender.clone(),
            };
            self.interruption_receiver
                .recv()
                .map_err(|_| anyhow::anyhow!("test interruption sender disconnected"))?;
            anyhow::bail!("старый read unwind-нулся к whole-parser restart boundary");
        }
        if self.packet_emitted {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        self.packet_emitted = true;
        Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            self.position,
            None,
            true,
            Bytes::from_static(&[0x65]),
        )))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.position = request.timestamp;
        self.packet_emitted = false;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.lifecycle_sender
            .send(InterruptibleReadLifecycleEvent::ReplacementStarted)
            .expect("test lifecycle receiver должен жить");
        if cancellation.is_cancelled() {
            return Err(MediaDemuxError::SeekCancelled.into());
        }
        if self.replacement_fails {
            anyhow::bail!("scripted replacement failure до commit");
        }
        self.seek_with_request(request)
    }
}

/// Создаёт fake и наружные probes без доступа progressive к concrete state.
fn interruptible_receipted_seek_demuxer(
    replacement_fails: bool,
) -> (
    InterruptibleReceiptedSeekDemuxer,
    Arc<TestActiveReadInterrupter>,
    Receiver<InterruptibleReadLifecycleEvent>,
) {
    let (interruption_sender, interruption_receiver) = sync_channel(1);
    let (lifecycle_sender, lifecycle_receiver) = sync_channel(4);
    let interruption_controller = Arc::new(TestActiveReadInterrupter {
        read_is_active: AtomicBool::new(false),
        request_count: AtomicUsize::new(0),
        interruption_sender,
    });
    let interruption_port = DemuxActiveReadInterruptionPort::new(interruption_controller.clone());
    (
        InterruptibleReceiptedSeekDemuxer {
            interruption_controller: Arc::clone(&interruption_controller),
            interruption_port,
            interruption_receiver,
            lifecycle_sender,
            replacement_fails,
            first_read: true,
            position: Duration::ZERO,
            packet_emitted: false,
        },
        interruption_controller,
        lifecycle_receiver,
    )
}

impl Demuxer for BlockingChannelDemuxer {
    /// Focused test не моделирует track discovery.
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    /// Streaming timeline неизвестна.
    fn duration(&self) -> Option<Duration> {
        None
    }

    /// Input явно non-seekable.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::UnknownTimeline,
        }
    }

    /// Блокируется как реальный parser/network reader до следующего event-а.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        self.receiver
            .recv()
            .map_err(|_| anyhow::anyhow!("test event sender disconnected"))
    }

    /// Non-seekable fake не должен получать seek.
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("test streaming demuxer is not seekable"))
    }
}

impl Demuxer for GatedSeekableEventDemuxer {
    /// Focused synchronization test не моделирует track discovery.
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    /// Конечная duration делает seek contract явным.
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    /// Deferred constructor обязан принять fake как seekable inner.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Сначала публикует rendezvous, затем ждёт exact test-owned event.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        // Текущий sequence сохраняется до инкремента.
        let read_sequence = self.next_read_sequence;
        // Следующий read получает новую identity.
        self.next_read_sequence += 1;
        // Zero-capacity channel доказывает, что test увидел начатый inner read.
        self.read_started
            .send(read_sequence)
            .map_err(|_| anyhow::anyhow!("test read observer disconnected"))?;
        // Второй rendezvous не даёт worker-у завершить read раньше test action.
        self.event_receiver
            .recv()
            .map_err(|_| anyhow::anyhow!("test event sender disconnected"))
    }

    /// Legacy seek делегирует typed request boundary.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Exact seek result совпадает с controller preview.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        // Fake сохраняет authoritative worker position.
        self.position = request.timestamp;
        // Result не вносит скрытый clamp либо decode-point offset.
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(self.position),
            actual_track_timestamp: None,
        })
    }
}

/// Endless fake позволяет заполнить queue и доказать отмену blocked producer-а.
struct CountingPacketDemuxer {
    /// Число выполненных blocking-owner reads.
    read_count: Arc<AtomicUsize>,
}

impl Demuxer for CountingPacketDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::UnknownTimeline,
        }
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let sequence = self.read_count.fetch_add(1, Ordering::SeqCst);
        Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            Duration::from_millis(sequence as u64),
            None,
            true,
            Bytes::from_static(&[0x55]),
        )))
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("test streaming demuxer is not seekable"))
    }
}

/// Seekable fake доказывает, что deferred wrapper не скрывает seek contract.
struct SeekableDeferredDemuxer;

impl Demuxer for SeekableDeferredDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!(
            "deferred seekable fake не должен дойти до seek"
        ))
    }
}

/// Seekable fake подтверждает latest-generation command и EOF wakeup.
struct CommandSeekableDemuxer {
    position: Duration,
    packet_emitted: bool,
}

#[derive(Clone, Copy)]
enum ControlledFirstSeekOutcome {
    Failure,
    MismatchedAnchor,
}

/// Первый seek блокируется с управляемым outcome, пока test публикует новую generation.
struct SlowControlledSeekDemuxer {
    first_seek_started: SyncSender<()>,
    release_first_seek: Receiver<()>,
    first_seek_outcome: ControlledFirstSeekOutcome,
    seek_count: usize,
    position: Duration,
    packet_emitted: bool,
}

impl Demuxer for SlowControlledSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.seek_count == 0 || self.packet_emitted {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        self.packet_emitted = true;
        Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            self.position,
            None,
            true,
            Bytes::from_static(&[1]),
        )))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count = self.seek_count.saturating_add(1);
        if self.seek_count == 1 {
            self.first_seek_started
                .send(())
                .expect("test receiver lives");
            self.release_first_seek
                .recv()
                .expect("test releases first seek");
            match self.first_seek_outcome {
                ControlledFirstSeekOutcome::Failure => {
                    anyhow::bail!("superseded seek failure");
                }
                ControlledFirstSeekOutcome::MismatchedAnchor => {
                    return Ok(DemuxSeekResult {
                        requested_position: MediaTime::from_duration(request.timestamp),
                        actual_position: MediaTime::from_duration(
                            request.timestamp + Duration::from_secs(1),
                        ),
                        actual_track_timestamp: None,
                    });
                }
            }
        }
        self.position = request.timestamp;
        self.packet_emitted = false;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Blocking read падает уже после публикации новой seek generation.
struct SupersededReadFailureDemuxer {
    read_started: SyncSender<()>,
    release_read: Receiver<()>,
    first_read: bool,
    position: Duration,
    packet_emitted: bool,
}

/// Seekable fake возвращает authoritative anchor, отличный от requested timestamp.
struct OffsetReceiptSeekDemuxer {
    /// Счётчик доказывает, что stale fence не дошёл до inner.
    seek_count: Arc<AtomicUsize>,
    /// Отдельный счётчик ловит утечку receipted-команды в legacy/preview boundary.
    ordinary_seek_count: Arc<AtomicUsize>,
}

impl Demuxer for OffsetReceiptSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.ordinary_seek_count.fetch_add(1, Ordering::SeqCst);
        let actual_position = request.timestamp.saturating_sub(Duration::from_secs(2));
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count.fetch_add(1, Ordering::SeqCst);
        let actual_position = request.timestamp.saturating_sub(Duration::from_secs(1));
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        })
    }
}

/// Первый receipt seek блокируется; следующие выполняются сразу.
struct SlowReceiptSeekDemuxer {
    /// Worker сообщает момент ownership первого command-а.
    first_seek_started: SyncSender<()>,
    /// Test освобождает первый blocking seek после enqueue новых intents.
    release_first_seek: Receiver<()>,
    /// Число выполненных seek commands.
    seek_count: usize,
}

/// Первый seek ждёт только request-scoped cancellation; второй сразу завершается.
struct CancellableReceiptSeekDemuxer {
    /// Test наблюдает начало каждого worker-owned seek без polling.
    seek_started: SyncSender<usize>,
    /// Число вызовов принадлежит единственному demux worker-у.
    seek_count: usize,
}

impl Demuxer for CancellableReceiptSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count = self.seek_count.saturating_add(1);
        self.seek_started
            .send(self.seek_count)
            .expect("test receiver должен жить");
        if self.seek_count == 1 {
            cancellation.wait_cancelled();
            return Err(MediaDemuxError::SeekCancelled.into());
        }
        self.seek_with_request(request)
    }
}

impl Demuxer for SlowReceiptSeekDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count = self.seek_count.saturating_add(1);
        if self.seek_count == 1 {
            self.first_seek_started
                .send(())
                .expect("test receiver должен жить");
            self.release_first_seek
                .recv()
                .expect("test обязан освободить blocking seek");
        }
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Первый seek падает транзакционно, второй подтверждает живой worker.
struct FirstReceiptSeekFailsDemuxer {
    /// Число вызовов выбирает scripted outcome.
    seek_count: usize,
}

/// Моделирует HLS replacement, который сохраняет request token в committed source.
struct CompletedTokenReplacementDemuxer {
    /// Последний committed source проверяет, не отравил ли его поздний supersede.
    committed_source_token: Option<DemuxSeekCancellationToken>,
    /// Второй request падает, а первый и третий успешно заменяют source.
    seek_count: usize,
}

impl Demuxer for CompletedTokenReplacementDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self
            .committed_source_token
            .as_ref()
            .is_some_and(DemuxSeekCancellationToken::is_cancelled)
        {
            anyhow::bail!("late supersede отравил committed source token");
        }
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count = self.seek_count.saturating_add(1);
        if self.seek_count == 2 {
            anyhow::bail!("scripted failure после первого committed replacement");
        }
        match cancellation.complete() {
            DemuxSeekCancellationCompletion::Completed => {
                self.committed_source_token = Some(cancellation);
                self.seek_with_request(request)
            }
            DemuxSeekCancellationCompletion::CancellationWon => {
                Err(MediaDemuxError::SeekCancelled.into())
            }
        }
    }
}

impl Demuxer for FirstReceiptSeekFailsDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek_count = self.seek_count.saturating_add(1);
        if self.seek_count == 1 {
            anyhow::bail!("scripted transactional seek failure");
        }
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

impl Demuxer for SupersededReadFailureDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.first_read {
            self.first_read = false;
            self.read_started.send(()).expect("test receiver lives");
            self.release_read.recv().expect("test releases read");
            anyhow::bail!("superseded read failure");
        }
        if self.packet_emitted {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        self.packet_emitted = true;
        Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            self.position,
            None,
            true,
            Bytes::from_static(&[1]),
        )))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.position = request.timestamp;
        self.packet_emitted = false;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

impl Demuxer for CommandSeekableDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.packet_emitted {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        self.packet_emitted = true;
        Ok(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            self.position,
            None,
            true,
            Bytes::from_static(&[1]),
        )))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.position = request.timestamp;
        self.packet_emitted = false;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Строит explicit маленькие limits для focused tests.
fn limits(max_events: usize, max_bytes: usize) -> ProgressiveDemuxBufferLimits {
    ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(max_events).expect("test event limit положителен"),
        NonZeroUsize::new(max_bytes).expect("test byte limit положителен"),
    )
}

/// Использует минимальный уже проверенный S21R retry interval.
fn retry_hint() -> DemuxRetryHint {
    DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER)
        .expect("minimum retry hint обязан быть валиден")
}

/// Создаёт blocking fake и его test-owned sender.
fn blocking_demuxer() -> (SyncSender<DemuxReadEvent>, Box<dyn Demuxer + Send>) {
    let (sender, receiver) = sync_channel(4);
    (sender, Box::new(BlockingChannelDemuxer { receiver }))
}

/// Создаёт zero-capacity read/event rendezvous и seekable fake.
fn gated_seekable_demuxer() -> (
    Receiver<usize>,
    SyncSender<DemuxReadEvent>,
    Box<dyn Demuxer + Send>,
) {
    // Read-start rendezvous не буферизует ненаблюдаемый worker progress.
    let (read_started_sender, read_started_receiver) = sync_channel(0);
    // Event rendezvous освобождает только уже наблюдаемый inner read.
    let (event_sender, event_receiver) = sync_channel(0);
    // Fake целиком передаётся deferred worker-у.
    let inner = Box::new(GatedSeekableEventDemuxer {
        read_started: read_started_sender,
        event_receiver,
        next_read_sequence: 1,
        position: Duration::ZERO,
    });
    // Test owner сохраняет обе control endpoints.
    (read_started_receiver, event_sender, inner)
}

/// Создаёт exact preview controller без clamp или скрытого offset.
fn exact_seek_controller() -> ProgressiveSeekController {
    ProgressiveSeekController::new(|request| {
        // Preview совпадает с fake worker result byte-for-byte по времени.
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    })
}

/// Ждёт конкретный gated read и возвращает точную timeout diagnostics.
fn wait_for_gated_read(read_started: &Receiver<usize>, expected_sequence: usize) {
    // Bounded receive не превращает regression в зависший test process.
    let actual_sequence = read_started
        .recv_timeout(Duration::from_secs(1))
        .expect("seekable worker обязан начать ожидаемый inner read");
    // Sequence защищает test orchestration от пропущенного либо лишнего read-а.
    assert_eq!(actual_sequence, expected_sequence);
}

/// Не выпускает test, пока worker terminal state не опубликован.
fn wait_until_worker_stopped(progressive: &ProgressiveDemuxer) {
    // Общий deadline ограничивает lifecycle failure одной секундой.
    let stop_deadline = Instant::now() + Duration::from_secs(1);
    // Queue guard читает тот же authoritative state, который пишет worker.
    let mut queue = progressive.shared.lock_queue();
    // Spurious Condvar wake не считается terminal completion.
    while !queue.worker_stopped {
        // Истёкший deadline даёт точный lifecycle failure.
        assert!(
            Instant::now() < stop_deadline,
            "progressive worker не опубликовал worker_stopped до deadline"
        );
        // Оставшийся budget не продлевается после spurious wake.
        let remaining = stop_deadline.saturating_duration_since(Instant::now());
        // Worker вызывает notify_all после mark_worker_stopped.
        let wait_result = progressive
            .shared
            .capacity_available
            .wait_timeout(queue, remaining);
        // Poison не скрывает terminal state при test failure.
        let (next_queue, timeout_result) = match wait_result {
            Ok(result) => result,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Следующая итерация повторно проверяет authoritative flag.
        queue = next_queue;
        // Timeout допустим только если terminal flag установлен одновременно.
        assert!(
            !timeout_result.timed_out() || queue.worker_stopped,
            "progressive worker не завершился внутри lifecycle timeout"
        );
    }
}

/// Poll helper не скрывает production scheduling: он нужен только test thread-у.
fn poll_until_event(progressive: &mut ProgressiveDemuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match progressive.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            event => return Ok(event),
        }
    }
}

/// Строит typed fence без magic identities внутри test cases.
fn receipt_fence(runtime_generation: u64, request_id: u64) -> ProgressiveSeekFence {
    ProgressiveSeekFence {
        runtime_generation: ProgressiveRuntimeGeneration::new(runtime_generation),
        request_id: ProgressiveSeekRequestId::new(request_id),
    }
}

/// Создаёт seekable receipt runtime и сохраняет control handle до type erasure.
fn receipted_runtime(
    inner: Box<dyn Demuxer + Send>,
    cancellation: CancellationToken,
    maximum_outstanding_receipts: usize,
) -> (ProgressiveDemuxer, ProgressiveAsyncSeekHandle) {
    let progressive = ProgressiveDemuxer::new_receipted_seekable(
        inner,
        cancellation,
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(maximum_outstanding_receipts).expect("test bound ненулевой"),
        ),
    )
    .expect("receipt worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("receipt capability опубликована");
    (progressive, handle)
}

/// Ждёт только в test thread-е; production poll остаётся строго nonblocking.
fn poll_until_receipt(handle: &ProgressiveAsyncSeekHandle) -> ProgressiveAsyncSeekReceipt {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(receipt) = handle.poll_receipt() {
            return receipt;
        }
        assert!(
            Instant::now() < deadline,
            "worker обязан опубликовать terminal receipt"
        );
        thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
    }
}
