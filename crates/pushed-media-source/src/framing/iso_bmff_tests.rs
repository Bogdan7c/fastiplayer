//! Тесты сканера fMP4 на настоящем фрагментированном MP4 из фикстур протокола.

use bytes::Bytes;

use super::{IsoBmffFramer, read_box_header};
use crate::framing::{FramedUnit, FramingError, FramingLimits, SegmentKind};

/// fMP4 H.264: `ftyp` 28 + `moov` 705, затем два фрагмента `moof`+`mdat`.
const VIDEO_MP4: &[u8] =
    include_bytes!("../../../browser-bridge-protocol/fixtures/media/video.mp4");
/// Границы сегментов в `VIDEO_MP4` (проверено разбором заголовков).
const INIT_END: usize = 733;
const FIRST_FRAGMENT_END: usize = 5714;

const LIMITS: FramingLimits = FramingLimits {
    max_initialization_bytes: 64 * 1024,
    max_media_bytes: 64 * 1024,
};

/// Скармливает поток кусками заданного размера и собирает сегменты.
fn frame_in_chunks(stream: &[u8], chunk_len: usize) -> Vec<FramedUnit> {
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    for chunk in stream.chunks(chunk_len) {
        framer.push(chunk, &mut completed).expect("valid fMP4");
    }
    assert_eq!(framer.held_bytes(), 0, "whole stream must be released");
    completed
}

fn box_bytes(box_type: &[u8; 4], body_len: usize) -> Vec<u8> {
    let mut bytes = u32::try_from(8 + body_len)
        .expect("small box")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(box_type);
    bytes.extend(std::iter::repeat_n(0xAB, body_len));
    bytes
}

#[test]
fn any_chunking_yields_exactly_init_and_two_fragments() {
    let expected = vec![
        FramedUnit::Initialization(Bytes::from_static(&VIDEO_MP4[..INIT_END])),
        FramedUnit::Media(Bytes::from_static(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END])),
        FramedUnit::Media(Bytes::from_static(&VIDEO_MP4[FIRST_FRAGMENT_END..])),
    ];
    for chunk_len in [1, 3, 7, 8, 100, 4096, VIDEO_MP4.len()] {
        assert_eq!(
            frame_in_chunks(VIDEO_MP4, chunk_len),
            expected,
            "chunk {chunk_len}"
        );
    }
}

#[test]
fn service_boxes_between_segments_are_dropped_even_when_split() {
    let mut stream = VIDEO_MP4[..INIT_END].to_vec();
    stream.extend(box_bytes(b"styp", 16));
    stream.extend(box_bytes(b"sidx", 300));
    stream.extend_from_slice(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END]);
    stream.extend(box_bytes(b"free", 5));
    for chunk_len in [1, 13, 333] {
        let units = frame_in_chunks(&stream, chunk_len);
        assert_eq!(units.len(), 2, "chunk {chunk_len}");
        assert_eq!(
            units[1],
            FramedUnit::Media(Bytes::from_static(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END]))
        );
    }
}

#[test]
fn abort_drops_half_received_fragment_and_next_fragment_still_frames() {
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    framer
        .push(&VIDEO_MP4[..INIT_END + 500], &mut completed)
        .expect("valid");
    assert_eq!(completed.len(), 1);
    assert_eq!(framer.held_bytes(), 500);
    framer.reset();
    assert_eq!(framer.held_bytes(), 0);
    framer
        .push(&VIDEO_MP4[FIRST_FRAGMENT_END..], &mut completed)
        .expect("valid");
    assert_eq!(
        completed.last(),
        Some(&FramedUnit::Media(Bytes::from_static(
            &VIDEO_MP4[FIRST_FRAGMENT_END..]
        )))
    );
}

#[test]
fn media_order_violations_are_typed_errors() {
    let mut completed = Vec::new();
    let mut framer = IsoBmffFramer::new(LIMITS);
    assert_eq!(
        framer.push(&box_bytes(b"mdat", 4), &mut completed),
        Err(FramingError::MediaDataWithoutFragmentHeader)
    );
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut two_headers = box_bytes(b"moof", 4);
    two_headers.extend(box_bytes(b"moof", 4));
    assert_eq!(
        framer.push(&two_headers, &mut completed),
        Err(FramingError::FragmentHeaderWithoutMediaData)
    );
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut header_then_init = box_bytes(b"moof", 4);
    header_then_init.extend_from_slice(&VIDEO_MP4[..INIT_END]);
    assert_eq!(
        framer.push(&header_then_init, &mut completed),
        Err(FramingError::FragmentHeaderWithoutMediaData)
    );
    assert!(completed.is_empty());
}

#[test]
fn oversized_box_is_rejected_from_its_header_before_the_body_arrives() {
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    let huge_header = [0x00, 0x10, 0x00, 0x00, b'm', b'd', b'a', b't'];
    framer
        .push(&box_bytes(b"moof", 4), &mut completed)
        .expect("header");
    assert_eq!(
        framer.push(&huge_header, &mut completed),
        Err(FramingError::SegmentTooLarge {
            kind: SegmentKind::Media,
            declared: 0x0010_0000 + 12,
            limit: LIMITS.max_media_bytes,
        })
    );
}

#[test]
fn malformed_box_sizes_are_rejected() {
    assert_eq!(
        read_box_header(&[0, 0, 0, 0, b'm', b'o', b'o', b'v']),
        Err(FramingError::BoxSizeUntilEndOfFile)
    );
    assert_eq!(
        read_box_header(&[0, 0, 0, 4, b'm', b'o', b'o', b'v']),
        Err(FramingError::BoxSizeTooSmall { declared: 4 })
    );
    let large = [0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 40];
    let header = read_box_header(&large).expect("valid").expect("complete");
    assert_eq!((header.header_len, header.total_len), (16, 40));
    assert_eq!(read_box_header(&large[..12]), Ok(None));
}

#[test]
fn finish_reports_and_drops_incomplete_segment() {
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    framer
        .push(&VIDEO_MP4[..INIT_END + 10], &mut completed)
        .expect("valid");
    assert_eq!(framer.finish(), 10);
    assert_eq!(framer.held_bytes(), 0);
}
