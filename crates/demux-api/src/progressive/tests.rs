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

use super::{
    ProgressiveDemuxBufferLimits, ProgressiveDemuxPacketTooLargeError,
    ProgressiveDemuxStartupError, ProgressiveDemuxer, ProgressiveSeekController,
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
        Ok(DemuxReadEvent::Packet(Packet::new(
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
        Ok(DemuxReadEvent::Packet(Packet::new(
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
        Ok(DemuxReadEvent::Packet(Packet::new(
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
