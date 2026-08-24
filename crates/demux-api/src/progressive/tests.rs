use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    MediaTime, Packet, TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind,
};
use source_core::CancellationToken;

use super::worker::{ProgressiveSeekCommand, ProgressiveSharedState, wait_for_seek_command};
use super::{
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveAsyncSeekReceipt, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxPacketTooLargeError, ProgressiveDemuxStartupError, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekController, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};

/// Blocking fake сохраняет главный production invariant: inner read может ждать сколько угодно.
struct BlockingChannelDemuxer {
    /// Test owner публикует готовые exact demux events.
    receiver: Receiver<DemuxReadEvent>,
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

/// Первый seek блокируется и падает, чтобы test успел supersede его новой generation.
struct SlowFailingSeekDemuxer {
    first_seek_started: SyncSender<()>,
    release_first_seek: Receiver<()>,
    seek_count: usize,
    position: Duration,
    packet_emitted: bool,
}

impl Demuxer for SlowFailingSeekDemuxer {
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
            anyhow::bail!("superseded seek failure");
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
    let mut progressive = ProgressiveDemuxer::new_deferred(
        || {
            thread::sleep(Duration::from_millis(40));
            Err(anyhow::anyhow!("deferred-open-test-failure"))
        },
        CancellationToken::new(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("deferred worker starts");

    let started_at = Instant::now();
    assert!(matches!(
        progressive.next_event().expect("readiness event"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    assert!(started_at.elapsed() < Duration::from_millis(20));

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match progressive.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                assert!(Instant::now() < deadline, "deferred failure timed out");
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            Err(error) => {
                assert_eq!(error.to_string(), "deferred-open-test-failure");
                break;
            }
            Ok(other) => panic!("unexpected deferred event: {other:?}"),
        }
    }
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
            Ok(Box::new(SlowFailingSeekDemuxer {
                first_seek_started: seek_started_sender,
                release_first_seek: release_receiver,
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
