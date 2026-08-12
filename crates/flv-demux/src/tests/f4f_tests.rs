use std::collections::VecDeque;

use bytes::Bytes;
use demux_api::{
    DemuxInput, OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind,
    OrderedSegmentReadError, OrderedSegmentSequence, OrderedSegmentSource,
};
use media_core::{DemuxReadEvent, Demuxer};
use source_core::CancellationToken;

use super::{FlvDemuxOptions, FlvDemuxer, avcc, flv_tag, legacy_avc_frame, legacy_avc_sequence};

#[test]
fn proven_fragment_extracts_headerless_flv_tags_from_mdat() {
    let media_payload = flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)));
    let source = FixtureSegments::new(vec![f4f_media(
        4,
        OrderedSegmentDiscontinuity::Continuous,
        media_payload,
    )]);
    let demuxer = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(source)),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("F4F opens");
    assert_eq!(demuxer.tracks()[0].codec_id, "V_MPEG4/ISO/AVC");
}

#[test]
fn optional_inline_bootstrap_is_accepted_only_when_valid() {
    let media_payload = flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)));
    let valid_source = FixtureSegments::new(vec![f4f_media_with_inline_bootstrap(
        5,
        OrderedSegmentDiscontinuity::Continuous,
        media_payload.clone(),
        f4f_abst(),
    )]);
    let valid_demuxer = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(valid_source)),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("spec-complete F4F fragment with valid inline bootstrap opens");
    assert_eq!(valid_demuxer.tracks()[0].codec_id, "V_MPEG4/ISO/AVC");

    let malformed_bootstrap = iso_box(b"abst", &[]);
    let malformed_source = FixtureSegments::new(vec![f4f_media_with_inline_bootstrap(
        6,
        OrderedSegmentDiscontinuity::Continuous,
        media_payload,
        malformed_bootstrap,
    )]);
    let error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(malformed_source)),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("inline bootstrap must not bypass structural validation");
    assert!(error.to_string().contains("abst"));
}

#[test]
fn rejects_standalone_bootstrap_and_incomplete_fragment_topology() {
    let standalone_bootstrap = OrderedSegment {
        sequence: OrderedSegmentSequence::new(4),
        kind: OrderedSegmentKind::Initialization,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: f4f_abst(),
    };
    let bootstrap_error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(FixtureSegments::new(vec![standalone_bootstrap]))),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("network bootstrap is outside the F4F adapter boundary");
    assert!(bootstrap_error.to_string().contains("initialization"));

    let mut incomplete_bytes = Vec::new();
    incomplete_bytes.extend_from_slice(&f4f_moof());
    incomplete_bytes.extend_from_slice(&iso_box(
        b"mdat",
        &flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
    ));
    let incomplete = OrderedSegment {
        sequence: OrderedSegmentSequence::new(5),
        kind: OrderedSegmentKind::Media,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: Bytes::from(incomplete_bytes),
    };
    let topology_error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(FixtureSegments::new(vec![incomplete]))),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("partial moof/mdat shape must not masquerade as F4F");
    assert!(topology_error.to_string().contains("afra"));
}

#[test]
fn rejects_unknown_and_duplicate_top_level_boxes() {
    let media_payload = flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)));

    let mut unknown_box_bytes = Vec::new();
    unknown_box_bytes.extend_from_slice(&f4f_afra());
    unknown_box_bytes.extend_from_slice(&f4f_moof());
    unknown_box_bytes.extend_from_slice(&iso_box(b"mdat", &media_payload));
    unknown_box_bytes.extend_from_slice(&iso_box(b"free", &[]));
    let unknown_box_error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(FixtureSegments::new(vec![f4f_media_from_bytes(
            6,
            OrderedSegmentDiscontinuity::Continuous,
            unknown_box_bytes,
        )]))),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("unknown top-level box must not masquerade as optional bootstrap");
    assert!(unknown_box_error.to_string().contains("optional abst"));

    let mut duplicate_mdat_bytes = Vec::new();
    duplicate_mdat_bytes.extend_from_slice(&f4f_afra());
    duplicate_mdat_bytes.extend_from_slice(&f4f_moof());
    duplicate_mdat_bytes.extend_from_slice(&iso_box(b"mdat", &media_payload));
    duplicate_mdat_bytes.extend_from_slice(&iso_box(b"mdat", &media_payload));
    let duplicate_mdat_error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(FixtureSegments::new(vec![f4f_media_from_bytes(
            7,
            OrderedSegmentDiscontinuity::Continuous,
            duplicate_mdat_bytes,
        )]))),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("duplicate mandatory top-level box must be rejected");
    assert!(duplicate_mdat_error.to_string().contains("ровно по одному"));
}

#[test]
fn nested_boxes_share_one_fragment_wide_budget() {
    let source = FixtureSegments::new(vec![f4f_media_with_inline_bootstrap(
        7,
        OrderedSegmentDiscontinuity::Continuous,
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        f4f_abst(),
    )]);
    let options = FlvDemuxOptions {
        fragment_boxes: crate::FlvLimit::new(9, "fragment_boxes").expect("non-zero limit"),
        ..FlvDemuxOptions::default()
    };
    let error = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(source)),
        true,
        CancellationToken::new(),
        options,
    )
    .err()
    .expect("ten nested and top-level boxes must exceed a nine-box budget");
    assert!(error.to_string().contains("всём fragment"));
}

#[test]
fn enforces_exact_sequence_and_discontinuity_config_lifecycle() {
    let first_media = f4f_media(
        10,
        OrderedSegmentDiscontinuity::Continuous,
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
    );
    let mut restarted_payload = flv_tag(9, 100, &legacy_avc_sequence(&avcc(30)));
    restarted_payload.extend_from_slice(&flv_tag(9, 120, &legacy_avc_frame(0, true)));
    let restarted_media = f4f_media(
        11,
        OrderedSegmentDiscontinuity::StartsNewTimeline,
        restarted_payload,
    );
    let source = FixtureSegments::new(vec![first_media, restarted_media]);
    let mut demuxer = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(source)),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("F4F opens");
    assert!(matches!(
        demuxer.next_event().expect("restart event"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let packet = demuxer.next_event().expect("restart keyframe");
    assert!(matches!(packet, DemuxReadEvent::Packet(_)));

    let wrong_sequence = FixtureSegments::new(vec![
        f4f_media(
            20,
            OrderedSegmentDiscontinuity::Continuous,
            flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        ),
        f4f_media(
            22,
            OrderedSegmentDiscontinuity::Continuous,
            flv_tag(9, 10, &legacy_avc_frame(0, true)),
        ),
    ]);
    let mut gap_demuxer = FlvDemuxer::open(
        DemuxInput::ordered_segments(Box::new(wrong_sequence)),
        true,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("first contiguous fragment opens");
    let error = gap_demuxer
        .next_event()
        .expect_err("sequence gap must fail before the next packet");
    assert!(matches!(
        error.downcast_ref::<crate::FlvDemuxError>(),
        Some(crate::FlvDemuxError::SegmentSequence { .. })
    ));
}

fn f4f_media(
    sequence: u64,
    discontinuity: OrderedSegmentDiscontinuity,
    flv_tags: Vec<u8>,
) -> OrderedSegment {
    assemble_f4f_media(sequence, discontinuity, flv_tags, None)
}

/// Собирает совместимый вариант, в котором bootstrap повторён внутри media fragment.
fn f4f_media_with_inline_bootstrap(
    sequence: u64,
    discontinuity: OrderedSegmentDiscontinuity,
    flv_tags: Vec<u8>,
    inline_bootstrap: Bytes,
) -> OrderedSegment {
    assemble_f4f_media(sequence, discontinuity, flv_tags, Some(inline_bootstrap))
}

/// Собирает реальный HDS media envelope; bootstrap по умолчанию принадлежит provider-у.
fn assemble_f4f_media(
    sequence: u64,
    discontinuity: OrderedSegmentDiscontinuity,
    flv_tags: Vec<u8>,
    inline_bootstrap: Option<Bytes>,
) -> OrderedSegment {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&f4f_afra());
    if let Some(inline_bootstrap) = inline_bootstrap {
        bytes.extend_from_slice(&inline_bootstrap);
    }
    bytes.extend_from_slice(&f4f_moof());
    bytes.extend_from_slice(&iso_box(b"mdat", &flv_tags));
    f4f_media_from_bytes(sequence, discontinuity, bytes)
}

/// Упаковывает готовые top-level box-ы в media segment с безопасным default lifecycle.
fn f4f_media_from_bytes(
    sequence: u64,
    discontinuity: OrderedSegmentDiscontinuity,
    bytes: Vec<u8>,
) -> OrderedSegment {
    OrderedSegment {
        sequence: OrderedSegmentSequence::new(sequence),
        kind: OrderedSegmentKind::Media,
        discontinuity,
        bytes: Bytes::from(bytes),
    }
}

fn f4f_afra() -> Bytes {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    iso_box(b"afra", &payload)
}

fn f4f_abst() -> Bytes {
    let mut segment_table = vec![0, 0, 0, 0, 0];
    segment_table.extend_from_slice(&1_u32.to_be_bytes());
    segment_table.extend_from_slice(&1_u32.to_be_bytes());
    segment_table.extend_from_slice(&1_u32.to_be_bytes());

    let mut fragment_table = vec![0, 0, 0, 0];
    fragment_table.extend_from_slice(&1_000_u32.to_be_bytes());
    fragment_table.push(0);
    fragment_table.extend_from_slice(&1_u32.to_be_bytes());
    fragment_table.extend_from_slice(&1_u32.to_be_bytes());
    fragment_table.extend_from_slice(&0_u64.to_be_bytes());
    fragment_table.extend_from_slice(&1_000_u32.to_be_bytes());

    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&[0, 0, 0, 0, 0]);
    payload.push(1);
    payload.extend_from_slice(&iso_box(b"asrt", &segment_table));
    payload.push(1);
    payload.extend_from_slice(&iso_box(b"afrt", &fragment_table));
    iso_box(b"abst", &payload)
}

fn f4f_moof() -> Bytes {
    let mut movie_header = vec![0, 0, 0, 0];
    movie_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_header = vec![0, 0, 0, 0];
    track_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_run = vec![0, 0, 0, 0];
    track_run.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_fragment = Vec::new();
    track_fragment.extend_from_slice(&iso_box(b"tfhd", &track_header));
    track_fragment.extend_from_slice(&iso_box(b"trun", &track_run));
    let mut payload = Vec::new();
    payload.extend_from_slice(&iso_box(b"mfhd", &movie_header));
    payload.extend_from_slice(&iso_box(b"traf", &track_fragment));
    iso_box(b"moof", &payload)
}

fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Bytes {
    let size = u32::try_from(8 + payload.len()).expect("fixture box fits u32");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(payload);
    Bytes::from(bytes)
}

struct FixtureSegments {
    segments: VecDeque<OrderedSegment>,
}

impl FixtureSegments {
    fn new(segments: Vec<OrderedSegment>) -> Self {
        Self {
            segments: segments.into(),
        }
    }
}

impl OrderedSegmentSource for FixtureSegments {
    fn next_segment(
        &mut self,
        _cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        Ok(self.segments.pop_front())
    }
}
