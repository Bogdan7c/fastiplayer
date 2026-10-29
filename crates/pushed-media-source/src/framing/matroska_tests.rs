//! Тесты сканера WebM на настоящем WebM VP9 + Opus из фикстур протокола.

use bytes::Bytes;

use super::{CLUSTER_ID, MatroskaFramer, TRACKS_ID};
use crate::framing::ebml::{
    EbmlElementSize, UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field, read_element_header,
};
use crate::framing::{FramedUnit, FramingError, FramingLimits};

/// WebM: EBML-заголовок, `Segment` известного размера, SeekHead, Void, Info,
/// Tracks, Tags, один `Cluster`, `Cues`.
const MUXED_WEBM: &[u8] =
    include_bytes!("../../../browser-bridge-protocol/fixtures/media/muxed.webm");

const LIMITS: FramingLimits = FramingLimits {
    max_initialization_bytes: 64 * 1024,
    max_media_bytes: 64 * 1024,
};

/// Элемент уровня `Segment`: ID и диапазон байтов в файле.
struct SegmentChild {
    id: u32,
    start: usize,
    end: usize,
}

/// Разбирает фикстуру независимо от сканера: EBML-заголовок, `Segment`, его дети.
fn layout() -> (usize, Vec<SegmentChild>) {
    let ebml = read_element_header(MUXED_WEBM)
        .expect("valid")
        .expect("header");
    let ebml_end = usize::try_from(ebml.total_len().expect("known")).expect("small");
    let segment = read_element_header(&MUXED_WEBM[ebml_end..])
        .expect("valid")
        .expect("header");
    let mut position = ebml_end + segment.header_len;
    let mut children = Vec::new();
    while position < MUXED_WEBM.len() {
        let child = read_element_header(&MUXED_WEBM[position..])
            .expect("valid")
            .expect("header");
        let end = position + usize::try_from(child.total_len().expect("known")).expect("small");
        children.push(SegmentChild {
            id: child.id,
            start: position,
            end,
        });
        position = end;
    }
    (ebml_end, children)
}

fn cluster_bytes() -> &'static [u8] {
    let (_, children) = layout();
    let cluster = children
        .iter()
        .find(|child| child.id == CLUSTER_ID)
        .expect("cluster");
    &MUXED_WEBM[cluster.start..cluster.end]
}

fn frame(stream: &[u8], chunk_len: usize) -> (Vec<FramedUnit>, MatroskaFramer) {
    let mut framer = MatroskaFramer::new(LIMITS);
    let mut completed = Vec::new();
    for chunk in stream.chunks(chunk_len) {
        framer.push(chunk, &mut completed).expect("valid WebM");
    }
    (completed, framer)
}

#[test]
fn init_keeps_only_info_and_tracks_with_unknown_segment_size_and_cluster_is_intact() {
    let (ebml_end, children) = layout();
    let kept: Vec<u8> = children
        .iter()
        .filter(|child| matches!(child.id, 0x1549_A966 | TRACKS_ID))
        .flat_map(|child| MUXED_WEBM[child.start..child.end].to_vec())
        .collect();
    let mut expected_init = MUXED_WEBM[..ebml_end].to_vec();
    expected_init.extend(encode_id(0x1853_8067));
    expected_init.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    expected_init.extend(kept);

    for chunk_len in [1, 5, 64, 1000, MUXED_WEBM.len()] {
        let (units, mut framer) = frame(MUXED_WEBM, chunk_len);
        assert_eq!(
            units,
            vec![
                FramedUnit::Initialization(Bytes::from(expected_init.clone())),
                FramedUnit::Media(Bytes::from_static(cluster_bytes())),
            ],
            "chunk {chunk_len}"
        );
        // Cues после кластера пропущены, ничего не удерживается.
        assert_eq!(framer.held_bytes(), 0);
        let mut tail = Vec::new();
        assert_eq!(framer.finish(&mut tail), 0);
        assert!(tail.is_empty());
    }
}

#[test]
fn unknown_size_cluster_ends_at_next_cluster_or_stream_end_and_gets_known_size() {
    let (ebml_end, children) = layout();
    let first_cluster_start = children
        .iter()
        .find(|child| child.id == CLUSTER_ID)
        .expect("cluster")
        .start;
    let cluster = cluster_bytes();
    let cluster_header = read_element_header(cluster)
        .expect("valid")
        .expect("header");
    let body = &cluster[cluster_header.header_len..];
    let mut unknown_size_cluster = encode_id(CLUSTER_ID);
    unknown_size_cluster.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    unknown_size_cluster.extend_from_slice(body);

    let mut stream = MUXED_WEBM[..first_cluster_start].to_vec();
    stream.extend_from_slice(&unknown_size_cluster);
    stream.extend_from_slice(&unknown_size_cluster);
    let _ = ebml_end;

    let mut expected_cluster = encode_id(CLUSTER_ID);
    expected_cluster.extend_from_slice(&encode_known_size_field(body.len() as u64).expect("small"));
    expected_cluster.extend_from_slice(body);

    for chunk_len in [1, 17, stream.len()] {
        let (mut units, mut framer) = frame(&stream, chunk_len);
        // Первый кластер закончился, когда начался второй; второй — только с концом потока.
        assert_eq!(units.len(), 2, "chunk {chunk_len}");
        assert_eq!(framer.finish(&mut units), 0);
        assert_eq!(units.len(), 3);
        for unit in &units[1..] {
            assert_eq!(
                unit,
                &FramedUnit::Media(Bytes::from(expected_cluster.clone()))
            );
        }
        let rewritten = match &units[1] {
            FramedUnit::Media(bytes) => read_element_header(bytes).expect("valid").expect("header"),
            FramedUnit::Initialization(_) => unreachable!("media"),
        };
        assert_eq!(rewritten.size, EbmlElementSize::Known(body.len() as u64));
    }
}

#[test]
fn second_init_after_clusters_is_framed_again() {
    let mut stream = MUXED_WEBM.to_vec();
    stream.extend_from_slice(MUXED_WEBM);
    let (units, _) = frame(&stream, 333);
    let kinds: Vec<&str> = units
        .iter()
        .map(|unit| match unit {
            FramedUnit::Initialization(_) => "init",
            FramedUnit::Media(_) => "media",
        })
        .collect();
    assert_eq!(kinds, ["init", "media", "init", "media"]);
}

#[test]
fn abort_after_init_accepts_a_cluster_directly() {
    let mut framer = MatroskaFramer::new(LIMITS);
    let mut completed = Vec::new();
    let cluster = cluster_bytes();
    let cluster_offset = MUXED_WEBM.len() - cluster.len() - 40; // Cues = 40 байтов в конце.
    framer
        .push(&MUXED_WEBM[..cluster_offset + 100], &mut completed)
        .expect("valid");
    assert_eq!(completed.len(), 1);
    framer.reset();
    assert_eq!(framer.held_bytes(), 0);
    framer.push(cluster, &mut completed).expect("valid");
    assert_eq!(
        completed.last(),
        Some(&FramedUnit::Media(Bytes::from_static(cluster)))
    );
}

#[test]
fn malformed_streams_are_typed_errors() {
    let mut completed = Vec::new();
    assert_eq!(
        MatroskaFramer::new(LIMITS).push(cluster_bytes(), &mut completed),
        Err(FramingError::MissingEbmlHeader)
    );
    let (ebml_end, children) = layout();
    let mut without_segment = MUXED_WEBM[..ebml_end].to_vec();
    without_segment.extend_from_slice(cluster_bytes());
    assert_eq!(
        MatroskaFramer::new(LIMITS).push(&without_segment, &mut completed),
        Err(FramingError::MissingSegment)
    );
    let segment_header_end = children.first().expect("children").start;
    let mut without_tracks = MUXED_WEBM[..segment_header_end].to_vec();
    without_tracks.extend_from_slice(cluster_bytes());
    assert_eq!(
        MatroskaFramer::new(LIMITS).push(&without_tracks, &mut completed),
        Err(FramingError::MissingTracks)
    );
    assert!(completed.is_empty());
}

#[test]
fn oversized_cluster_is_rejected_from_its_header() {
    let small_limits = FramingLimits {
        max_initialization_bytes: 64 * 1024,
        max_media_bytes: 1024,
    };
    let mut framer = MatroskaFramer::new(small_limits);
    let mut completed = Vec::new();
    let cluster_offset = MUXED_WEBM.len() - cluster_bytes().len() - 40;
    let result = framer.push(&MUXED_WEBM[..cluster_offset + 12], &mut completed);
    assert!(
        matches!(result, Err(FramingError::SegmentTooLarge { .. })),
        "{result:?}"
    );
}
