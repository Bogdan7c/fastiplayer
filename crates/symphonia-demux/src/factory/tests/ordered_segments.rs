use std::collections::VecDeque;
use std::error::Error;
use std::num::NonZeroUsize;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    DemuxFactoryOpenError, DemuxHints, DemuxInput, DemuxOpenError, DemuxRegistry, DemuxSniffBudget,
    OrderedSegment, OrderedSegmentKind, OrderedSegmentReadError, OrderedSegmentSequence,
    OrderedSegmentSource,
};
use media_core::{DemuxReadEvent, DemuxSeekability, Demuxer, TrackKind};
use source_core::CancellationToken;

use super::super::SymphoniaDemuxFactory;
use crate::{DemuxError, DemuxerOptions, OrderedSegmentLifecycleError};

/// FFmpeg 8.1 generated AAC fMP4: empty-moov init + три independent moof/mdat fragments.
/// Тест использует первые два media fragments; external encoder в test runtime не вызывается.
const GENERATED_FRAGMENTED_M4A_BASE64: &str = "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";

/// Source сохраняет exact segment objects; optional call отменяет shared operation.
struct MemoryOrderedSource {
    /// Ещё не выданные segments.
    segments: VecDeque<OrderedSegment>,
    /// Номер source call-а, который должен вернуть cancellation.
    cancel_on_call: Option<usize>,
    /// Текущее число source calls, включая registry sniff.
    calls: usize,
}

impl OrderedSegmentSource for MemoryOrderedSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        self.calls += 1;
        if self.cancel_on_call == Some(self.calls) {
            cancellation.cancel();
        }
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        Ok(self.segments.pop_front())
    }
}

/// Декодирует test-only fixture без production dependency.
pub(super) fn decode_base64_fixture() -> Vec<u8> {
    let mut decoded = Vec::with_capacity(GENERATED_FRAGMENTED_M4A_BASE64.len() * 3 / 4);
    let mut accumulator = 0_u32;
    let mut accumulated_bits = 0_u8;
    for encoded_byte in GENERATED_FRAGMENTED_M4A_BASE64.bytes() {
        let value = match encoded_byte {
            b'A'..=b'Z' => encoded_byte - b'A',
            b'a'..=b'z' => encoded_byte - b'a' + 26,
            b'0'..=b'9' => encoded_byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => panic!("generated fixture содержит invalid base64 byte"),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        accumulated_bits += 6;
        if accumulated_bits >= 8 {
            accumulated_bits -= 8;
            decoded.push((accumulator >> accumulated_bits) as u8);
            accumulator &= (1_u32 << accumulated_bits) - 1;
        }
    }
    decoded
}

/// Разделяет immutable generated corpus по заранее проверенным top-level boundaries.
pub(super) fn generated_segments() -> (Bytes, Bytes, Bytes) {
    const INIT_END: usize = 709;
    const FIRST_MEDIA_END: usize = 1_350;
    const SECOND_MEDIA_END: usize = 1_658;

    let fixture = decode_base64_fixture();
    assert!(fixture.len() >= SECOND_MEDIA_END);
    assert_eq!(&fixture[4..8], b"ftyp");
    assert_eq!(&fixture[32..36], b"moov");
    assert_eq!(&fixture[INIT_END + 4..INIT_END + 8], b"moof");
    assert_eq!(&fixture[FIRST_MEDIA_END + 4..FIRST_MEDIA_END + 8], b"moof");

    (
        Bytes::copy_from_slice(&fixture[..INIT_END]),
        Bytes::copy_from_slice(&fixture[INIT_END..FIRST_MEDIA_END]),
        Bytes::copy_from_slice(&fixture[FIRST_MEDIA_END..SECOND_MEDIA_END]),
    )
}

/// Создаёт segment без изменения generated bytes.
pub(super) fn segment(sequence: u64, kind: OrderedSegmentKind, bytes: Bytes) -> OrderedSegment {
    OrderedSegment {
        sequence: OrderedSegmentSequence::new(sequence),
        kind,
        bytes,
    }
}

/// Открывает ordered input через production registry/factory и one-segment sniff replay.
pub(super) fn open_ordered(
    segments: Vec<OrderedSegment>,
    cancellation: CancellationToken,
    cancel_on_call: Option<usize>,
) -> Result<Box<dyn Demuxer + Send>, DemuxOpenError> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory"),
        ))
        .expect("register factory");
    registry.open(
        DemuxInput::ordered_segments(Box::new(MemoryOrderedSource {
            segments: segments.into(),
            cancel_on_call,
            calls: 0,
        })),
        DemuxHints::none(),
        DemuxSniffBudget::new(
            NonZeroUsize::new(4_096).expect("sniff bytes"),
            NonZeroUsize::MIN,
            Duration::from_secs(1),
        )
        .expect("sniff budget"),
        cancellation,
    )
}

/// Ищет concrete lifecycle error по всей Symphonia/anyhow source chain.
fn find_lifecycle_error<'a>(
    mut error: &'a (dyn Error + 'static),
) -> Option<&'a OrderedSegmentLifecycleError> {
    loop {
        if let Some(lifecycle_error) = error.downcast_ref::<OrderedSegmentLifecycleError>() {
            return Some(lifecycle_error);
        }
        if let Some(DemuxError::OrderedSegmentLifecycle(lifecycle_error)) =
            error.downcast_ref::<DemuxError>()
        {
            return Some(lifecycle_error);
        }
        error = error.source()?;
    }
}

/// Lifecycle может проявиться при eager open либо при первом runtime read.
fn assert_lifecycle_failure(segments: Vec<OrderedSegment>, expected: OrderedSegmentLifecycleError) {
    match open_ordered(segments, CancellationToken::never_cancelled(), None) {
        Err(error) => assert_eq!(
            find_lifecycle_error(&error),
            Some(&expected),
            "unexpected open error: {error:#?}"
        ),
        Ok(mut demuxer) => {
            for _ in 0..8 {
                match demuxer.next_event() {
                    Err(error) => {
                        assert_eq!(find_lifecycle_error(error.as_ref()), Some(&expected));
                        return;
                    }
                    Ok(DemuxReadEvent::EndOfStream) => {
                        panic!("invalid lifecycle не должен выглядеть как EOF")
                    }
                    Ok(_) => {}
                }
            }
            panic!("lifecycle error не появилась в bounded read attempts")
        }
    }
}

#[test]
fn ordered_fmp4_opens_without_hint_and_reads_multiple_media_fragments() {
    let (init, first_media, second_media) = generated_segments();
    let mut demuxer = open_ordered(
        vec![
            segment(10, OrderedSegmentKind::Initialization, init),
            segment(20, OrderedSegmentKind::Media, first_media),
            segment(40, OrderedSegmentKind::Media, second_media),
        ],
        CancellationToken::never_cancelled(),
        None,
    )
    .expect("open ordered fMP4 without hints");

    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert_eq!(demuxer.duration(), None);
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("generated AAC track");
    assert_eq!(audio_track.duration, None);
    assert!(
        audio_track
            .codec_private
            .as_ref()
            .is_some_and(|codec_private| !codec_private.is_empty())
    );

    let mut packet_count = 0;
    for _ in 0..16 {
        match demuxer.next_event().expect("read ordered fMP4") {
            DemuxReadEvent::Packet(packet) => {
                packet_count += 1;
                assert_eq!(packet.kind, TrackKind::Audio);
                assert!(packet.track_pts.is_some());
                assert!(packet.track_dts.is_some());
                assert!(packet.duration.is_some_and(|duration| !duration.is_zero()));
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::MediaMetadataChanged(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite ordered demuxer не должен публиковать readiness")
            }
        }
    }
    assert_eq!(packet_count, 2);
    assert!(demuxer.seek(Duration::ZERO).is_err());
}

#[test]
fn ordered_factory_surfaces_typed_lifecycle_failures() {
    let (init, first_media, second_media) = generated_segments();
    assert_lifecycle_failure(
        vec![segment(7, OrderedSegmentKind::Media, first_media.clone())],
        OrderedSegmentLifecycleError::MediaBeforeInitialization { sequence: 7 },
    );
    assert_lifecycle_failure(
        vec![
            segment(1, OrderedSegmentKind::Initialization, init.clone()),
            segment(2, OrderedSegmentKind::Initialization, init.clone()),
        ],
        OrderedSegmentLifecycleError::RepeatedInitializationSegment { sequence: 2 },
    );

    for invalid_sequence in [2, 1] {
        assert_lifecycle_failure(
            vec![
                segment(0, OrderedSegmentKind::Initialization, init.clone()),
                segment(2, OrderedSegmentKind::Media, first_media.clone()),
                segment(
                    invalid_sequence,
                    OrderedSegmentKind::Media,
                    second_media.clone(),
                ),
            ],
            OrderedSegmentLifecycleError::NonIncreasingSequence {
                previous_sequence: 2,
                current_sequence: invalid_sequence,
            },
        );
    }
}

#[test]
fn ordered_factory_preserves_cancellation_during_sniff_and_open() {
    let (init, first_media, _) = generated_segments();
    let cancelled_before_sniff = CancellationToken::new();
    cancelled_before_sniff.cancel();
    let sniff_error = match open_ordered(
        vec![segment(0, OrderedSegmentKind::Initialization, init.clone())],
        cancelled_before_sniff,
        None,
    ) {
        Err(error) => error,
        Ok(_) => panic!("cancel before sniff must reject the open request"),
    };
    assert!(matches!(
        sniff_error,
        DemuxOpenError::ProbeRejected(demux_api::DemuxProbeRejection::Cancelled)
    ));

    let cancelled_during_open = CancellationToken::new();
    let open_error = match open_ordered(
        vec![
            segment(0, OrderedSegmentKind::Initialization, init),
            segment(1, OrderedSegmentKind::Media, first_media),
        ],
        cancelled_during_open,
        Some(2),
    ) {
        Err(error) => error,
        Ok(_) => panic!("cancel during ordered open must reject the open request"),
    };
    assert!(matches!(
        open_error,
        DemuxOpenError::FactoryRejected {
            source: DemuxFactoryOpenError::Cancelled,
            ..
        }
    ));
}
