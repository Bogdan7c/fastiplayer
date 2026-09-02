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
    ProgressiveDemuxStartupError, ProgressiveDemuxer, ProgressiveRuntimeGeneration,
    ProgressiveSeekController, ProgressiveSeekFence, ProgressiveSeekRequestId,
};

mod preview_cancellation;
mod sync_preview_receipt;

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

#[test]
fn blocked_inner_read_returns_readiness_without_blocking_player_owner() {
    let (sender, inner) = blocking_demuxer();
    let cancellation = CancellationToken::new();
    let mut progressive =
        ProgressiveDemuxer::new(inner, cancellation, limits(2, 1024), retry_hint())
            .expect("progressive worker starts");

    let started_at = Instant::now();
    let first = progressive.next_event().expect("readiness event");
    assert!(matches!(first, DemuxReadEvent::TemporarilyUnavailable(_)));
    assert!(started_at.elapsed() < Duration::from_millis(100));

    sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker receiver lives");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("terminal event"),
        DemuxReadEvent::EndOfStream
    ));
}

#[test]
fn deferred_open_failure_is_nonblocking_and_preserves_typed_error() {
    let (release_sender, release_receiver) = sync_channel(0);
    let mut progressive = ProgressiveDemuxer::new_deferred(
        move || {
            release_receiver
                .recv()
                .expect("test owner releases deferred failure");
            Err(anyhow::anyhow!("deferred-open-test-failure"))
        },
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts");
    let readiness = progressive.readiness_port();

    let started_at = Instant::now();
    assert!(matches!(
        progressive.next_event().expect("readiness event"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    assert!(started_at.elapsed() < Duration::from_millis(20));

    release_sender
        .send(())
        .expect("deferred worker still waits for release");
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::EventAvailable,
        "queued typed failure обязан иметь приоритет над worker terminal state"
    );
    let error = progressive
        .next_event()
        .expect_err("deferred worker publishes exact failure");
    assert_eq!(error.to_string(), "deferred-open-test-failure");
}

#[test]
fn readiness_port_wakes_on_tracks_changed_without_consuming_event() {
    let (sender, inner) = blocking_demuxer();
    let mut progressive = ProgressiveDemuxer::new(
        inner,
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("progressive worker starts");
    let readiness = progressive.readiness_port();
    let (outcome_sender, outcome_receiver) = sync_channel(1);
    let waiter = thread::spawn(move || {
        outcome_sender
            .send(readiness.wait_until(Instant::now() + Duration::from_secs(1)))
            .expect("publish readiness outcome");
    });

    sender
        .send(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
            Vec::new(),
            None,
        )))
        .expect("worker receiver lives");
    assert_eq!(
        outcome_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("tracks publication wakes readiness waiter"),
        ProgressiveDemuxReadiness::EventAvailable
    );
    waiter.join().expect("join readiness waiter");
    assert!(matches!(
        progressive
            .next_event()
            .expect("tracks event remains queued"),
        DemuxReadEvent::TracksChanged(_)
    ));

    sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker receiver lives until terminal event");
}

#[test]
fn readiness_port_cancellation_wakes_without_waiting_for_deadline() {
    let (sender, inner) = blocking_demuxer();
    let cancellation = CancellationToken::new();
    let progressive =
        ProgressiveDemuxer::new(inner, cancellation.clone(), limits(2, 1024), retry_hint())
            .expect("progressive worker starts");
    let readiness = progressive.readiness_port();
    let (outcome_sender, outcome_receiver) = sync_channel(1);
    let waiter = thread::spawn(move || {
        outcome_sender
            .send(readiness.wait_until(Instant::now() + Duration::from_secs(30)))
            .expect("publish cancellation readiness outcome");
    });

    cancellation.cancel();
    assert_eq!(
        outcome_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation future wakes Condvar waiter"),
        ProgressiveDemuxReadiness::Cancelled
    );
    waiter.join().expect("join cancelled readiness waiter");
    // Если worker уже заметил cancellation до blocking read-а, receiver законно закрыт;
    // иначе drop sender-а освобождает scripted recv без ложного terminal event-а.
    drop(sender);
}

#[test]
fn readiness_port_cancellation_waiter_saturation_fails_closed_without_deadlock() {
    let shared = Arc::new(ProgressiveSharedState::new(limits(1, 1)));
    let cancellation = CancellationToken::new();
    let mut registered_futures = Vec::new();
    let mut registered_wakers = Vec::new();

    // source-core contract ограничивает один token восемью уникальными waker-ами;
    // девятый poll обязан отменить token fail-closed вместо unbounded registry.
    for _ in 0..8 {
        let waker = Waker::from(Arc::new(ReadinessCountingWake {
            wake_count: AtomicUsize::new(0),
        }));
        let mut cancellation_future = Box::pin(cancellation.cancelled());
        let mut context = Context::from_waker(&waker);
        assert!(cancellation_future.as_mut().poll(&mut context).is_pending());
        registered_wakers.push(waker);
        registered_futures.push(cancellation_future);
    }
    let readiness = ProgressiveDemuxReadinessPort::new(shared, cancellation.clone());
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::Cancelled
    );
    assert!(cancellation.is_cancelled());
    drop(registered_futures);
    drop(registered_wakers);
}

#[test]
fn readiness_port_observes_worker_panic_as_terminal_without_fake_event() {
    let mut progressive = ProgressiveDemuxer::new_deferred(
        || -> anyhow::Result<Box<dyn Demuxer + Send>> {
            panic!("readiness-test-worker-panic");
        },
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts before its scripted panic");
    let readiness = progressive.readiness_port();

    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::WorkerStopped
    );
    let error = progressive
        .next_event()
        .expect_err("panic не должен превращаться в synthetic event");
    assert!(
        error
            .downcast_ref::<super::ProgressiveDemuxWorkerStoppedError>()
            .is_some()
    );
}

#[test]
fn readiness_port_ignores_stale_generation_and_preserves_queue_accounting() {
    let shared = Arc::new(ProgressiveSharedState::new(limits(2, 16)));
    let cancellation = CancellationToken::new();
    let packet_bytes = Bytes::from_static(&[1, 2, 3, 4]);
    assert_eq!(
        push_progressive_message(
            &shared,
            &cancellation,
            0,
            ProgressiveMessage::Event(DemuxReadEvent::Packet(Packet::new_unbounded(
                TrackId::new(1),
                TrackKind::Video,
                Duration::ZERO,
                None,
                true,
                packet_bytes.clone(),
            ))),
        ),
        ProgressivePushOutcome::Published
    );
    let readiness = ProgressiveDemuxReadinessPort::new(Arc::clone(&shared), cancellation.clone());
    assert_eq!(
        readiness.wait_until(Instant::now() + Duration::from_secs(1)),
        ProgressiveDemuxReadiness::EventAvailable
    );
    {
        let queue = shared.lock_queue();
        assert_eq!(queue.messages.len(), 1, "port не потребляет queued event");
        assert_eq!(
            queue.queued_encoded_bytes,
            packet_bytes.len(),
            "port не меняет byte accounting"
        );
    }

    shared.lock_queue().current_generation = 1;
    assert_eq!(
        readiness.wait_until(Instant::now()),
        ProgressiveDemuxReadiness::DeadlineReached,
        "stale-only queue не является readiness нового player intent-а"
    );
    let queue = shared.lock_queue();
    assert_eq!(queue.messages.len(), 1);
    assert_eq!(queue.queued_encoded_bytes, packet_bytes.len());
}

#[test]
fn deferred_open_rejects_seekable_inner_instead_of_hiding_seekability() {
    let mut progressive = ProgressiveDemuxer::new_deferred(
        || Ok(Box::new(SeekableDeferredDemuxer)),
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts");

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match progressive.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                assert!(Instant::now() < deadline, "deferred rejection timed out");
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            Err(error) => {
                assert!(
                    error
                        .downcast_ref::<ProgressiveDemuxStartupError>()
                        .is_some_and(|source| {
                            matches!(source, ProgressiveDemuxStartupError::SeekableInput)
                        })
                );
                break;
            }
            Ok(other) => panic!("unexpected deferred event: {other:?}"),
        }
    }
}

#[test]
fn oversized_packet_fails_with_typed_bounded_error() {
    let (sender, inner) = blocking_demuxer();
    let mut progressive =
        ProgressiveDemuxer::new(inner, CancellationToken::new(), limits(2, 4), retry_hint())
            .expect("progressive worker starts");
    sender
        .send(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Bytes::from(vec![0_u8; 5]),
        )))
        .expect("worker receiver lives");

    let error = loop {
        match progressive.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            Err(error) => break error,
            Ok(other) => panic!("unexpected event: {other:?}"),
        }
    };
    let typed = error
        .downcast_ref::<ProgressiveDemuxPacketTooLargeError>()
        .expect("typed oversize source сохраняется");
    assert_eq!(typed.packet_bytes, 5);
    assert_eq!(typed.budget_bytes, 4);
}

#[test]
fn drop_cancels_worker_waiting_on_full_backpressure_queue() {
    let read_count = Arc::new(AtomicUsize::new(0));
    let inner = Box::new(CountingPacketDemuxer {
        read_count: Arc::clone(&read_count),
    });
    let progressive =
        ProgressiveDemuxer::new(inner, CancellationToken::new(), limits(1, 1), retry_hint())
            .expect("progressive worker starts");
    let shared = Arc::clone(&progressive.shared);

    let fill_deadline = Instant::now() + Duration::from_secs(1);
    while read_count.load(Ordering::SeqCst) < 2 && Instant::now() < fill_deadline {
        thread::yield_now();
    }
    assert_eq!(read_count.load(Ordering::SeqCst), 2);

    drop(progressive);
    let stop_deadline = Instant::now() + Duration::from_secs(1);
    while !shared.lock_queue().worker_stopped && Instant::now() < stop_deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(shared.lock_queue().worker_stopped);
}

/// Уже опубликованная отмена не позволяет EOF worker-у войти в timed wait.
#[test]
fn eof_wait_observes_preexisting_cancellation_without_blocking() {
    // Shared state использует production queue/Condvar boundary без фонового thread-а.
    let shared = ProgressiveSharedState::new(limits(1, 1));
    // Отмена устанавливается до wait, точно моделируя lost-wakeup guard.
    let cancellation = CancellationToken::new();
    // Production token выражает lifecycle intent без прямой мутации queue state.
    cancellation.cancel();

    // Guard обязан вернуть управление синхронно, не входя в timed wait.
    wait_for_seek_command(&shared, &cancellation);

    // Wait не меняет lifecycle state, которым владеет caller.
    assert!(cancellation.is_cancelled());
}

/// Уже опубликованный seek command не теряется внутри EOF wait boundary.
#[test]
fn eof_wait_observes_preexisting_seek_without_blocking() {
    // Shared state использует production queue/Condvar boundary без фонового thread-а.
    let shared = ProgressiveSharedState::new(limits(1, 1));
    // Exact request остаётся владельцем целевой позиции.
    let request = DemuxSeekRequest::accurate(Duration::from_secs(1));
    // Preview фиксирует уже подтверждённый player-owner anchor.
    let preview = DemuxSeekResult {
        requested_position: MediaTime::from_duration(request.timestamp),
        actual_position: MediaTime::from_duration(request.timestamp),
        actual_track_timestamp: None,
    };
    // Command публикуется до wait, точно моделируя lost-wakeup guard.
    shared.lock_queue().pending_seek = Some(ProgressiveSeekCommand::Previewed {
        generation: 1,
        request,
        preview,
        cancellation: DemuxSeekCancellationToken::new(),
    });
    // Неотменённый token заставляет проверку дойти именно до pending seek.
    let cancellation = CancellationToken::new();

    // Guard обязан вернуть управление синхронно, не входя в timed wait.
    wait_for_seek_command(&shared, &cancellation);

    // Wait не забирает command: его применяет только worker owner.
    assert!(shared.lock_queue().pending_seek.is_some());
}

/// Seekable TUA выполняет реальный timeout, затем cancellation завершает worker.
#[test]
fn seekable_tua_wait_observes_cancellation_and_stops_before_return() {
    // Zero-capacity channels делают каждый inner read наблюдаемым.
    let (read_started, event_sender, inner) = gated_seekable_demuxer();
    // Test owner сохраняет cancellation handle до terminal assertion.
    let cancellation = CancellationToken::new();
    // Deferred constructor запускает production seekable worker boundary.
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || Ok(inner),
        exact_seek_controller(),
        cancellation.clone(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("seekable worker starts");

    // Initial track publication освобождает worker до первого controlled read.
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    // Первый read уже принадлежит worker-у и ждёт test event.
    wait_for_gated_read(&read_started, 1);
    // 50 ms гарантированно больше production cancellation poll quantum 25 ms.
    let completed_retry_hint = DemuxRetryHint::new(Duration::from_millis(50))
        .expect("controlled retry hint обязан быть валиден");
    // Первая TUA проходит normal retry timeout без queue publication.
    event_sender
        .send(DemuxReadEvent::TemporarilyUnavailable(completed_retry_hint))
        .expect("worker ждёт первый controlled event");
    // Второй read доказывает, что wait_for_inner_retry завершил timeout path.
    wait_for_gated_read(&read_started, 2);

    // Длинный retry не может естественно истечь раньше cancellation.
    let cancelled_retry_hint = DemuxRetryHint::new(DemuxRetryHint::MAX_RETRY_AFTER)
        .expect("maximum retry hint обязан быть валиден");
    // Worker получает вторую TUA, оставаясь внутри уже начатого read path.
    event_sender
        .send(DemuxReadEvent::TemporarilyUnavailable(cancelled_retry_hint))
        .expect("worker ждёт второй controlled event");
    // Cancellation прерывает retry wait, а не публикует fake EOF/error.
    cancellation.cancel();
    // Test не возвращается раньше mark_worker_stopped.
    wait_until_worker_stopped(&progressive);
    // Ни одна inner TUA не должна попасть в bounded message queue.
    assert!(progressive.shared.lock_queue().messages.is_empty());
}

/// Gated old-generation event даёт Stale, а cancelled event даёт Stopped.
#[test]
fn seekable_stale_and_stopped_push_outcomes_are_synchronized() {
    // Один fake управляет старым, актуальным и cancelled reads.
    let (read_started, event_sender, inner) = gated_seekable_demuxer();
    // Cancellation handle нужен для exact Stopped push outcome.
    let cancellation = CancellationToken::new();
    // Worker использует production deferred seekable orchestration.
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || Ok(inner),
        exact_seek_controller(),
        cancellation.clone(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("seekable worker starts");

    // Initial tracks удаляются до generation race.
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    // Generation zero read блокируется до публикации нового seek intent.
    wait_for_gated_read(&read_started, 1);
    // Новый player intent немедленно меняет visible queue generation.
    let requested_position = Duration::from_secs(2);
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(requested_position))
        .expect("new generation seek accepted");
    // Старый EOF возвращается только после generation change.
    event_sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker ждёт stale controlled event");
    // Второй read возможен только после Stale drop и применения pending seek.
    wait_for_gated_read(&read_started, 2);
    // Stale event не должен занимать current-generation queue.
    assert!(progressive.shared.lock_queue().messages.is_empty());

    // Актуальный packet доказывает, что Stale outcome не остановил worker.
    event_sender
        .send(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            requested_position,
            None,
            true,
            Bytes::from_static(&[0x7a]),
        )))
        .expect("worker ждёт current-generation packet");
    // Player owner получает только packet актуальной generation.
    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("current-generation packet")
    else {
        panic!("current-generation packet expected");
    };
    // Exact timestamp подтверждает применение authoritative seek.
    assert_eq!(packet.pts, requested_position);

    // Третий read блокирует worker внутри parser boundary.
    wait_for_gated_read(&read_started, 3);
    // Cancellation устанавливается до возврата controlled event-а.
    cancellation.cancel();
    // Event после cancellation обязан получить Stopped, а не Published.
    event_sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker ждёт cancelled controlled event");
    // Test ждёт exact mark_worker_stopped notification.
    wait_until_worker_stopped(&progressive);
    // Cancelled event не должен остаться скрытым terminal message-ом.
    assert!(progressive.shared.lock_queue().messages.is_empty());
}

#[test]
fn seekable_worker_wakes_after_eof_and_drops_superseded_generation_output() {
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        || {
            Ok(Box::new(CommandSeekableDemuxer {
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial packet"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial EOF"),
        DemuxReadEvent::EndOfStream
    ));

    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(8)))
        .expect("first command");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(2)))
        .expect("latest command");
    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("post-seek packet")
    else {
        panic!("latest seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(2));
}

#[test]
fn stale_failing_seek_does_not_stop_worker_before_latest_command() {
    assert_stale_controlled_seek_does_not_stop_latest_command(ControlledFirstSeekOutcome::Failure);
}

#[test]
fn stale_mismatched_seek_does_not_stop_worker_before_latest_command() {
    assert_stale_controlled_seek_does_not_stop_latest_command(
        ControlledFirstSeekOutcome::MismatchedAnchor,
    );
}

fn assert_stale_controlled_seek_does_not_stop_latest_command(
    first_seek_outcome: ControlledFirstSeekOutcome,
) {
    let (seek_started_sender, seek_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || {
            Ok(Box::new(SlowControlledSeekDemuxer {
                first_seek_started: seek_started_sender,
                release_first_seek: release_receiver,
                first_seek_outcome,
                seek_count: 0,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial EOF"),
        DemuxReadEvent::EndOfStream
    ));

    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(8)))
        .expect("first preview");
    seek_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered first seek");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(2)))
        .expect("latest preview");
    release_sender.send(()).expect("release first seek");

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("latest packet after stale failure")
    else {
        panic!("latest seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(2));
}

#[test]
fn stale_read_failure_does_not_stop_worker_before_pending_seek() {
    let (read_started_sender, read_started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || {
            Ok(Box::new(SupersededReadFailureDemuxer {
                read_started: read_started_sender,
                release_read: release_receiver,
                first_read: true,
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    read_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker entered blocking read");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("pending seek preview");
    release_sender.send(()).expect("release stale read");

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("packet after stale read error")
    else {
        panic!("post-seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(3));
}

#[test]
fn legacy_runtime_rejects_async_seek_capability_without_changing_legacy_contract() {
    let (_sender, receiver) = sync_channel(1);
    let progressive = ProgressiveDemuxer::new(
        Box::new(BlockingChannelDemuxer { receiver }),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("legacy progressive runtime starts");

    let error = progressive
        .enqueue_async_seek(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect_err("legacy runtime не должен притворяться receipt-capable");
    assert_eq!(error, ProgressiveAsyncSeekEnqueueError::CapabilityAbsent);
}

#[test]
fn receipted_seek_publishes_authoritative_result_exactly_once() {
    let seek_count = Arc::new(AtomicUsize::new(0));
    let ordinary_seek_count = Arc::new(AtomicUsize::new(0));
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::clone(&seek_count),
            ordinary_seek_count: Arc::clone(&ordinary_seek_count),
        }),
        CancellationToken::new(),
        2,
    );
    let fence = receipt_fence(7, 1);
    handle
        .enqueue(fence, DemuxSeekRequest::accurate(Duration::from_secs(5)))
        .expect("valid request accepted");

    let receipt = poll_until_receipt(&handle);
    assert_eq!(receipt.fence, fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("authoritative success receipt expected");
    };
    assert_eq!(
        result.actual_position,
        MediaTime::from_duration(Duration::from_secs(4))
    );
    assert_eq!(seek_count.load(Ordering::SeqCst), 1);
    assert_eq!(ordinary_seek_count.load(Ordering::SeqCst), 0);
    assert_eq!(handle.poll_receipt(), None, "receipt is at-most-once");
}

#[test]
fn stale_fence_is_receipted_without_touching_inner_parser() {
    let seek_count = Arc::new(AtomicUsize::new(0));
    let ordinary_seek_count = Arc::new(AtomicUsize::new(0));
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::clone(&seek_count),
            ordinary_seek_count: Arc::clone(&ordinary_seek_count),
        }),
        CancellationToken::new(),
        2,
    );
    let stale_fence = receipt_fence(6, 1);
    handle
        .enqueue(
            stale_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("stale request получает terminal receipt");

    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence: stale_fence,
            outcome: ProgressiveAsyncSeekOutcome::Stale,
        }
    );
    assert_eq!(seek_count.load(Ordering::SeqCst), 0);
    assert_eq!(ordinary_seek_count.load(Ordering::SeqCst), 0);
}

#[test]
fn receipt_bound_and_monotonic_identity_are_enforced_until_drain() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(OffsetReceiptSeekDemuxer {
            seek_count: Arc::new(AtomicUsize::new(0)),
            ordinary_seek_count: Arc::new(AtomicUsize::new(0)),
        }),
        CancellationToken::new(),
        1,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        handle
            .enqueue(
                receipt_fence(7, 1),
                DemuxSeekRequest::accurate(Duration::from_secs(2)),
            )
            .expect_err("identity must increase"),
        ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity
    );
    assert_eq!(
        handle
            .enqueue(
                receipt_fence(7, 2),
                DemuxSeekRequest::accurate(Duration::from_secs(2)),
            )
            .expect_err("undrained receipt retains capacity"),
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull
    );

    let first_receipt = poll_until_receipt(&handle);
    assert_eq!(
        first_receipt.fence.request_id,
        ProgressiveSeekRequestId::new(1)
    );
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("drain releases exact capacity");
    assert_eq!(
        poll_until_receipt(&handle).fence.request_id,
        ProgressiveSeekRequestId::new(2)
    );
}

#[test]
fn rapid_seek_supersedes_in_flight_and_pending_requests() {
    let (started_sender, started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let (_progressive, handle) = receipted_runtime(
        Box::new(SlowReceiptSeekDemuxer {
            first_seek_started: started_sender,
            release_first_seek: release_receiver,
            seek_count: 0,
        }),
        CancellationToken::new(),
        3,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker owns first blocking seek");
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("second request accepted");
    handle
        .enqueue(
            receipt_fence(7, 3),
            DemuxSeekRequest::accurate(Duration::from_secs(3)),
        )
        .expect("third request supersedes pending second");
    release_sender.send(()).expect("release first seek");

    let mut outcomes = [None; 3];
    for _ in 0..3 {
        let receipt = poll_until_receipt(&handle);
        let index =
            usize::try_from(receipt.fence.request_id.value() - 1).expect("small test identity");
        outcomes[index] = Some(receipt.outcome);
    }
    assert_eq!(
        outcomes,
        [
            Some(ProgressiveAsyncSeekOutcome::Superseded),
            Some(ProgressiveAsyncSeekOutcome::Superseded),
            Some(ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
                requested_position: MediaTime::from_duration(Duration::from_secs(3)),
                actual_position: MediaTime::from_duration(Duration::from_secs(3)),
                actual_track_timestamp: None,
            })),
        ]
    );
    assert_eq!(handle.poll_receipt(), None);
}

#[test]
fn newer_receipted_seek_physically_cancels_in_flight_worker_operation() {
    let (started_sender, started_receiver) = sync_channel(2);
    let (_progressive, handle) = receipted_runtime(
        Box::new(CancellableReceiptSeekDemuxer {
            seek_started: started_sender,
            seek_count: 0,
        }),
        CancellationToken::new(),
        2,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(1),
        "worker должен войти в первый cancellable seek"
    );

    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("newer request accepted");
    assert_eq!(
        started_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(2),
        "второй seek должен стартовать без ручного release первого"
    );

    let first = poll_until_receipt(&handle);
    let second = poll_until_receipt(&handle);
    let outcomes = [
        (first.fence.request_id.value(), first.outcome),
        (second.fence.request_id.value(), second.outcome),
    ];
    assert!(outcomes.contains(&(1, ProgressiveAsyncSeekOutcome::Superseded)));
    assert!(outcomes.contains(&(
        2,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(Duration::from_secs(2)),
            actual_position: MediaTime::from_duration(Duration::from_secs(2)),
            actual_track_timestamp: None,
        })
    )));
}

#[test]
fn deferred_receipted_seek_interrupts_stalled_old_body_before_replacement_receipt() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(false);
    let progressive = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || Ok(Box::new(inner)),
        exact_seek_controller(),
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
        ProgressiveRuntimeGeneration::new(7),
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(2).expect("test receipt bound ненулевой"),
        ),
    )
    .expect("deferred receipt worker запускается");
    let handle = progressive
        .async_seek_handle()
        .expect("deferred runtime публикует receipt capability");

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted),
        "worker должен войти в stalled old body read"
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("current-runtime request accepted");
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::BodyDropped),
        "physical old body owner должен быть dropped после interruption"
    );
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReplacementStarted),
        "replacement нельзя начинать до whole-parser unwind"
    );
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        1
    );
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(DemuxSeekResult {
            actual_position,
            ..
        }) if actual_position == MediaTime::from_secs(5)
    ));
}

#[test]
fn failed_replacement_after_active_read_interruption_has_no_fake_success_and_restarts_old_source() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(true);
    let (mut progressive, handle) = receipted_runtime(Box::new(inner), CancellationToken::new(), 2);

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted)
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("current-runtime request accepted");
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::BodyDropped)
    );
    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReplacementStarted)
    );
    assert_eq!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Failed,
        "ошибка replacement не имеет права публиковать fake success"
    );
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        1
    );

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("old committed source должен restart-нуться")
    else {
        panic!("rollback обязан вернуть packet старого committed source-а");
    };
    assert_eq!(
        packet.pts,
        Duration::ZERO,
        "failed target нельзя подменять новой timeline position"
    );
}

#[test]
fn stale_receipted_seek_does_not_interrupt_current_active_read() {
    let (inner, interruption_controller, lifecycle_receiver) =
        interruptible_receipted_seek_demuxer(false);
    let (_progressive, handle) = receipted_runtime(Box::new(inner), CancellationToken::new(), 2);

    assert_eq!(
        lifecycle_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InterruptibleReadLifecycleEvent::ReadStarted)
    );
    let stale_fence = receipt_fence(6, 1);
    handle
        .enqueue(
            stale_fence,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("stale request получает terminal receipt");
    assert_eq!(
        interruption_controller.request_count.load(Ordering::SeqCst),
        0,
        "stale fence не должен трогать current physical read"
    );

    let _ = interruption_controller
        .request_active_read_interruption(DemuxActiveReadInterruptionReason::ReceiptedSeekEnqueued);
    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence: stale_fence,
            outcome: ProgressiveAsyncSeekOutcome::Stale,
        }
    );
}

#[test]
fn failed_receipted_seek_does_not_kill_transactional_worker() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(FirstReceiptSeekFailsDemuxer { seek_count: 0 }),
        CancellationToken::new(),
        2,
    );
    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
        )
        .expect("first request accepted");
    assert_eq!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Failed
    );
    handle
        .enqueue(
            receipt_fence(7, 2),
            DemuxSeekRequest::accurate(Duration::from_secs(2)),
        )
        .expect("worker remains available after transactional error");
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(_)
    ));
}

#[test]
fn completed_replacement_survives_later_failed_request_and_accepts_retry() {
    let (_progressive, handle) = receipted_runtime(
        Box::new(CompletedTokenReplacementDemuxer {
            committed_source_token: None,
            seek_count: 0,
        }),
        CancellationToken::new(),
        1,
    );

    for (request_id, target_seconds, expected_success) in
        [(1, 1, true), (2, 2, false), (3, 3, true)]
    {
        handle
            .enqueue(
                receipt_fence(7, request_id),
                DemuxSeekRequest::accurate(Duration::from_secs(target_seconds)),
            )
            .expect("worker должен принимать retry после terminal receipt");
        let receipt = poll_until_receipt(&handle);
        assert_eq!(receipt.fence.request_id.value(), request_id);
        assert_eq!(
            matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Succeeded(_)),
            expected_success,
            "scripted request обязан сохранить свой terminal outcome"
        );
    }
}

#[test]
fn async_seek_after_visible_eof_reopens_front_generation() {
    let (mut progressive, handle) = receipted_runtime(
        Box::new(CommandSeekableDemuxer {
            position: Duration::ZERO,
            packet_emitted: false,
        }),
        CancellationToken::new(),
        1,
    );

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial packet"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("visible EOF"),
        DemuxReadEvent::EndOfStream
    ));

    handle
        .enqueue(
            receipt_fence(7, 1),
            DemuxSeekRequest::accurate(Duration::from_secs(6)),
        )
        .expect("new generation accepted after EOF");
    assert!(matches!(
        poll_until_receipt(&handle).outcome,
        ProgressiveAsyncSeekOutcome::Succeeded(_)
    ));

    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("post-seek packet after old EOF")
    else {
        panic!("новая generation должна снять только старый EOS latch");
    };
    assert_eq!(packet.pts, Duration::from_secs(6));
}

#[test]
fn cancellation_terminalizes_in_flight_receipted_seek() {
    let (started_sender, started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let cancellation = CancellationToken::new();
    let (_progressive, handle) = receipted_runtime(
        Box::new(SlowReceiptSeekDemuxer {
            first_seek_started: started_sender,
            release_first_seek: release_receiver,
            seek_count: 0,
        }),
        cancellation.clone(),
        1,
    );
    let fence = receipt_fence(7, 1);
    handle
        .enqueue(fence, DemuxSeekRequest::accurate(Duration::from_secs(1)))
        .expect("request accepted");
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("worker owns blocking seek");
    cancellation.cancel();
    release_sender.send(()).expect("release cancelled seek");

    assert_eq!(
        poll_until_receipt(&handle),
        ProgressiveAsyncSeekReceipt {
            fence,
            outcome: ProgressiveAsyncSeekOutcome::Cancelled,
        }
    );
    assert_eq!(
        handle.poll_receipt(),
        None,
        "cancellation emits one receipt"
    );
}
