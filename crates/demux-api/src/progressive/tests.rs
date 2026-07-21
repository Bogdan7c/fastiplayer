use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, Demuxer, Packet,
    TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind,
};
use source_core::CancellationToken;

use super::{
    ProgressiveDemuxBufferLimits, ProgressiveDemuxPacketTooLargeError, ProgressiveDemuxer,
};

/// Blocking fake сохраняет главный production invariant: inner read может ждать сколько угодно.
struct BlockingChannelDemuxer {
    /// Test owner публикует готовые exact demux events.
    receiver: Receiver<DemuxReadEvent>,
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
        Ok(DemuxReadEvent::Packet(Packet::new(
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
fn oversized_packet_fails_with_typed_bounded_error() {
    let (sender, inner) = blocking_demuxer();
    let mut progressive =
        ProgressiveDemuxer::new(inner, CancellationToken::new(), limits(2, 4), retry_hint())
            .expect("progressive worker starts");
    sender
        .send(DemuxReadEvent::Packet(Packet::new(
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
