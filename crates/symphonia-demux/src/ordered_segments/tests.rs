use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use demux_api::{
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};
use source_core::CancellationToken;

use super::{OrderedSegmentLifecycleError, OrderedSegmentReader};

/// In-memory source сохраняет segment boundaries и может вернуть typed transport error.
struct MemorySegmentSource {
    /// Ordered read outcomes без скрытого byte concatenation.
    outcomes: VecDeque<Result<OrderedSegment, OrderedSegmentReadError>>,
    /// Drop observation доказывает отсутствие detached/background ownership.
    dropped: Option<Arc<AtomicBool>>,
}

impl OrderedSegmentSource for MemorySegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        self.outcomes.pop_front().transpose()
    }
}

impl Drop for MemorySegmentSource {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped {
            dropped.store(true, Ordering::SeqCst);
        }
    }
}

/// Создаёт один immutable segment с коротким test payload.
fn segment(sequence: u64, kind: OrderedSegmentKind, bytes: &'static [u8]) -> OrderedSegment {
    OrderedSegment {
        sequence: OrderedSegmentSequence::new(sequence),
        kind,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: Bytes::from_static(bytes),
    }
}

/// Строит reader над successful outcomes.
fn reader(segments: impl IntoIterator<Item = OrderedSegment>) -> OrderedSegmentReader {
    OrderedSegmentReader::new(
        Box::new(MemorySegmentSource {
            outcomes: segments.into_iter().map(Ok).collect(),
            dropped: None,
        }),
        CancellationToken::never_cancelled(),
    )
}

/// Извлекает concrete lifecycle error, сохранённую внутри `io::Error`.
fn lifecycle_error(error: &std::io::Error) -> &OrderedSegmentLifecycleError {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<OrderedSegmentLifecycleError>())
        .expect("typed ordered lifecycle error")
}

#[test]
fn reader_preserves_boundaries_and_accepts_strictly_increasing_gaps() {
    let mut reader = reader([
        segment(10, OrderedSegmentKind::Initialization, b"init"),
        segment(20, OrderedSegmentKind::Media, b"media-one"),
        segment(42, OrderedSegmentKind::Media, b"media-two"),
    ]);
    let mut destination = [0_u8; 32];

    let init_bytes = reader.read(&mut destination).expect("read init");
    assert_eq!(&destination[..init_bytes], b"init");
    let first_media_bytes = reader.read(&mut destination).expect("read first media");
    assert_eq!(&destination[..first_media_bytes], b"media-one");
    let second_media_bytes = reader.read(&mut destination).expect("read second media");
    assert_eq!(&destination[..second_media_bytes], b"media-two");
    assert_eq!(reader.read(&mut destination).expect("terminal EOF"), 0);
}

#[test]
fn discontinuity_fails_closed_before_installing_segment_bytes() {
    let mut discontinuous = segment(20, OrderedSegmentKind::Media, b"new-timeline");
    discontinuous.discontinuity = OrderedSegmentDiscontinuity::StartsNewTimeline;
    let mut reader = reader([
        segment(10, OrderedSegmentKind::Initialization, b"init"),
        discontinuous,
    ]);
    let mut destination = [0_u8; 32];
    assert_eq!(reader.read(&mut destination).expect("init"), b"init".len());

    let error = reader
        .read(&mut destination)
        .expect_err("finite Symphonia adapter must reject discontinuity");

    assert_eq!(
        lifecycle_error(&error),
        &OrderedSegmentLifecycleError::DiscontinuityRequiresSessionReset { sequence: 20 }
    );
}

#[test]
fn media_before_init_and_repeated_init_are_typed() {
    let mut media_first = reader([segment(7, OrderedSegmentKind::Media, b"media")]);
    let media_first_error = media_first
        .read(&mut [0_u8; 16])
        .expect_err("media before init");
    assert_eq!(
        lifecycle_error(&media_first_error),
        &OrderedSegmentLifecycleError::MediaBeforeInitialization { sequence: 7 }
    );

    let mut repeated_init = reader([
        segment(1, OrderedSegmentKind::Initialization, b"init"),
        segment(2, OrderedSegmentKind::Initialization, b"new-init"),
    ]);
    assert_eq!(
        repeated_init.read(&mut [0_u8; 16]).expect("first init"),
        b"init".len()
    );
    let repeated_error = repeated_init
        .read(&mut [0_u8; 16])
        .expect_err("repeated init");
    assert_eq!(
        lifecycle_error(&repeated_error),
        &OrderedSegmentLifecycleError::RepeatedInitializationSegment { sequence: 2 }
    );
}

#[test]
fn duplicate_and_decreasing_sequences_are_typed() {
    for invalid_sequence in [2, 1] {
        let mut reader = reader([
            segment(0, OrderedSegmentKind::Initialization, b"init"),
            segment(2, OrderedSegmentKind::Media, b"first-media"),
            segment(
                invalid_sequence,
                OrderedSegmentKind::Media,
                b"invalid-media",
            ),
        ]);
        let mut destination = [0_u8; 32];
        assert_eq!(reader.read(&mut destination).expect("init"), b"init".len());
        assert_eq!(
            reader.read(&mut destination).expect("first media"),
            b"first-media".len()
        );
        let error = reader
            .read(&mut destination)
            .expect_err("non-increasing sequence");
        assert_eq!(
            lifecycle_error(&error),
            &OrderedSegmentLifecycleError::NonIncreasingSequence {
                previous_sequence: 2,
                current_sequence: invalid_sequence,
            }
        );
    }
}

#[test]
fn missing_init_and_empty_segment_are_typed() {
    let mut missing_init = reader([]);
    let missing_error = missing_init.read(&mut [0_u8; 8]).expect_err("missing init");
    assert_eq!(
        lifecycle_error(&missing_error),
        &OrderedSegmentLifecycleError::MissingInitializationSegment
    );

    let mut empty_init = reader([segment(0, OrderedSegmentKind::Initialization, b"")]);
    let empty_error = empty_init.read(&mut [0_u8; 8]).expect_err("empty init");
    assert_eq!(
        lifecycle_error(&empty_error),
        &OrderedSegmentLifecycleError::EmptySegment {
            sequence: 0,
            kind: OrderedSegmentKind::Initialization,
        }
    );
}

#[test]
fn cancellation_source_failure_and_drop_keep_exact_outcomes() {
    let cancellation = CancellationToken::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let source = MemorySegmentSource {
        outcomes: VecDeque::from([Ok(segment(0, OrderedSegmentKind::Initialization, b"init"))]),
        dropped: Some(Arc::clone(&dropped)),
    };
    let mut cancelled_reader = OrderedSegmentReader::new(Box::new(source), cancellation.clone());
    cancellation.cancel();
    let cancelled = cancelled_reader
        .read(&mut [0_u8; 8])
        .expect_err("cancelled read");
    assert_eq!(cancelled.kind(), std::io::ErrorKind::Interrupted);
    assert!(matches!(
        cancelled
            .get_ref()
            .and_then(|source| source.downcast_ref::<OrderedSegmentReadError>()),
        Some(OrderedSegmentReadError::Cancelled)
    ));
    drop(cancelled_reader);
    assert!(dropped.load(Ordering::SeqCst));

    let mut failed_reader = OrderedSegmentReader::new(
        Box::new(MemorySegmentSource {
            outcomes: VecDeque::from([Err(OrderedSegmentReadError::Failed {
                reason: "bounded test failure".to_owned(),
            })]),
            dropped: None,
        }),
        CancellationToken::never_cancelled(),
    );
    let failed = failed_reader
        .read(&mut [0_u8; 8])
        .expect_err("source failure");
    assert_eq!(failed.kind(), std::io::ErrorKind::Other);
    assert!(matches!(
        failed
            .get_ref()
            .and_then(|source| source.downcast_ref::<OrderedSegmentReadError>()),
        Some(OrderedSegmentReadError::Failed { reason }) if reason == "bounded test failure"
    ));
}
