//! Тесты сканера fMP4 на настоящем фрагментированном MP4 из фикстур протокола.

use bytes::Bytes;

use super::{IsoBmffFramer, read_box_header};
use crate::framing::{FramedUnit, FramingError, FramingLimits, SegmentKind};
use crate::test_fixtures::pseudo_random_bytes;

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

/// Бокс с 64-битным размером (`size == 1`).
fn large_box_bytes(box_type: &[u8; 4], body_len: usize) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 1];
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(&(16 + body_len as u64).to_be_bytes());
    bytes.extend(std::iter::repeat_n(0xCD, body_len));
    bytes
}

/// Заголовок бокса: объявленный размер и тип, тела ещё нет.
fn box_header(declared: u32, box_type: [u8; 4]) -> Vec<u8> {
    let mut bytes = declared.to_be_bytes().to_vec();
    bytes.extend_from_slice(&box_type);
    bytes
}

/// Отдельный список, а не константа сканера: убрать тип из сканера обязано ломать тест.
const LEGITIMATE_SERVICE_BOXES: [&[u8; 4]; 14] = [
    b"free", b"skip", b"styp", b"sidx", b"emsg", b"prft", b"mfra", b"uuid", b"pdin", b"bloc",
    b"meta", b"meco", b"ssix", b"pssh",
];

/// Типы, которых нет на верхнем уровне потока MSE: двоичные, похожие на текст,
/// похожие на настоящий бокс, настоящие, но живущие внутри других боксов.
const GARBAGE_BOX_TYPES: [[u8; 4]; 6] = [
    [0x9A, 0x01, 0xFE, 0x33],
    *b"x7Qz",
    *b"MOOF",
    *b"mvhd",
    *b"trun",
    [0, 0, 0, 0],
];

/// Размер меньше всех пределов: «правдоподобно большой» случайный заголовок.
const IMPLAUSIBLE_SIZE_UNDER_THE_CAP: u32 = 20 * 1024;
/// Страница объявляет бокс больше, чем поток имеет право держать.
const THREE_GIB: u32 = 3 * 1024 * 1024 * 1024;

/// Все положения в потоке, где сканер читает заголовок бокса верхнего уровня:
/// первые байты, после init, после целого фрагмента, `moof` ждёт `mdat`.
fn streams_before_a_box_header() -> [Vec<u8>; 4] {
    let moof_len = usize::try_from(
        read_box_header(&VIDEO_MP4[INIT_END..])
            .expect("valid")
            .expect("complete")
            .total_len,
    )
    .expect("small");
    [
        Vec::new(),
        VIDEO_MP4[..INIT_END].to_vec(),
        VIDEO_MP4[..FIRST_FRAGMENT_END].to_vec(),
        VIDEO_MP4[..INIT_END + moof_len].to_vec(),
    ]
}

#[test]
fn every_service_box_browsers_accept_is_skipped_in_any_chunking() {
    let expected = vec![
        FramedUnit::Initialization(Bytes::from_static(&VIDEO_MP4[..INIT_END])),
        FramedUnit::Media(Bytes::from_static(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END])),
        FramedUnit::Media(Bytes::from_static(&VIDEO_MP4[FIRST_FRAGMENT_END..])),
    ];
    for service_type in LEGITIMATE_SERVICE_BOXES {
        // Перед init, между сегментами, после последнего; в обоих видах размера,
        // пустой и `uuid` c 16-байтовым номером производителя.
        let mut stream = box_bytes(service_type, 0);
        stream.extend_from_slice(&VIDEO_MP4[..INIT_END]);
        stream.extend(box_bytes(service_type, 16));
        stream.extend_from_slice(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END]);
        stream.extend(large_box_bytes(service_type, 300));
        stream.extend_from_slice(&VIDEO_MP4[FIRST_FRAGMENT_END..]);
        stream.extend(box_bytes(service_type, 40));
        for chunk_len in [1, 7, 13, 333, stream.len()] {
            assert_eq!(
                frame_in_chunks(&stream, chunk_len),
                expected,
                "{service_type:?} chunk {chunk_len}"
            );
        }
    }
}

#[test]
fn a_service_box_between_a_fragment_header_and_its_data_is_still_skipped() {
    let moof_end = streams_before_a_box_header()[3].len();
    let mut stream = VIDEO_MP4[..moof_end].to_vec();
    stream.extend(box_bytes(b"free", 8));
    stream.extend_from_slice(&VIDEO_MP4[moof_end..FIRST_FRAGMENT_END]);
    let units = frame_in_chunks(&stream, 11);
    assert_eq!(
        units.last(),
        Some(&FramedUnit::Media(Bytes::from_static(
            &VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END]
        )))
    );
}

#[test]
fn a_garbage_box_type_is_refused_at_its_header_wherever_it_lands() {
    for stream in streams_before_a_box_header() {
        for box_type in GARBAGE_BOX_TYPES {
            let mut framer = IsoBmffFramer::new(LIMITS);
            let mut completed = Vec::new();
            framer.push(&stream, &mut completed).expect("valid prefix");
            let units_before_garbage = completed.len();
            assert_eq!(
                framer.push(
                    &box_header(IMPLAUSIBLE_SIZE_UNDER_THE_CAP, box_type),
                    &mut completed
                ),
                Err(FramingError::UnrecognizedTopLevelBox { box_type }),
                "after {} bytes",
                stream.len()
            );
            assert_eq!(completed.len(), units_before_garbage, "nothing is released");
        }
    }
}

#[test]
fn a_garbage_header_split_across_chunks_is_refused_on_its_last_byte() {
    let header = box_header(IMPLAUSIBLE_SIZE_UNDER_THE_CAP, *b"x7Qz");
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    framer
        .push(&VIDEO_MP4[..INIT_END], &mut completed)
        .expect("valid init");
    let (last_byte, head) = header.split_last().expect("header");
    for byte in head {
        framer
            .push(std::slice::from_ref(byte), &mut completed)
            .expect("the header is not whole yet");
    }
    assert_eq!(
        framer.push(std::slice::from_ref(last_byte), &mut completed),
        Err(FramingError::UnrecognizedTopLevelBox { box_type: *b"x7Qz" })
    );
}

#[test]
fn random_bytes_are_never_waited_for_as_a_phantom_box() {
    // Случай стенда: 64 КиБ случайных байтов — на первых байтах и после init.
    for seed in 0..2_000 {
        let garbage = pseudo_random_bytes(seed, 64 * 1024);
        for stream in streams_before_a_box_header() {
            let mut framer = IsoBmffFramer::new(LIMITS);
            let mut completed = Vec::new();
            framer.push(&stream, &mut completed).expect("valid prefix");
            assert!(
                framer.push(&garbage, &mut completed).is_err(),
                "seed {seed} after {} bytes was swallowed",
                stream.len()
            );
        }
    }
}

#[test]
fn a_declared_three_gigabytes_are_refused_by_the_header_for_every_kind_of_box() {
    let refused = [
        (*b"free", SegmentKind::ServiceElement),
        (*b"uuid", SegmentKind::ServiceElement),
        (*b"ftyp", SegmentKind::Initialization),
        (*b"moov", SegmentKind::Initialization),
        (*b"moof", SegmentKind::Media),
    ];
    for (box_type, kind) in refused {
        let limit = match kind {
            SegmentKind::Initialization => LIMITS.max_initialization_bytes,
            SegmentKind::Media | SegmentKind::ServiceElement => LIMITS.max_media_bytes,
        };
        let expected = Err(FramingError::SegmentTooLarge {
            kind,
            declared: u64::from(THREE_GIB),
            limit,
        });
        let mut completed = Vec::new();
        let mut framer = IsoBmffFramer::new(LIMITS);
        assert_eq!(
            framer.push(&box_header(THREE_GIB, box_type), &mut completed),
            expected
        );
        // Тот же размер в 64-битной записи: тот же отказ.
        let mut large_form = vec![0, 0, 0, 1];
        large_form.extend_from_slice(&box_type);
        large_form.extend_from_slice(&u64::from(THREE_GIB).to_be_bytes());
        let mut framer = IsoBmffFramer::new(LIMITS);
        assert_eq!(framer.push(&large_form, &mut completed), expected);
        assert!(framer.held_bytes() <= large_form.len(), "no body is held");
    }
}

#[test]
fn a_service_box_up_to_the_limit_is_skipped_without_holding_its_bytes() {
    let mut stream = VIDEO_MP4[..INIT_END].to_vec();
    let at_the_limit = u32::try_from(LIMITS.max_media_bytes).expect("small limit");
    stream.extend(box_header(at_the_limit, *b"free"));
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    framer.push(&stream, &mut completed).expect("under the cap");
    // Страница капает настоящие байты: память не растёт ни на байт.
    for _ in 0..(LIMITS.max_media_bytes - 8) / 1024 {
        framer.push(&[0x5A; 1024], &mut completed).expect("body");
        assert_eq!(framer.held_bytes(), 0);
    }
    let rest = (LIMITS.max_media_bytes - 8) % 1024;
    framer.push(&vec![0x5A; rest], &mut completed).expect("end");
    framer
        .push(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END], &mut completed)
        .expect("the stream goes on after the box");
    assert_eq!(completed.len(), 2, "init and the fragment after the box");

    let mut framer = IsoBmffFramer::new(LIMITS);
    assert_eq!(
        framer.push(&box_header(at_the_limit + 1, *b"free"), &mut completed),
        Err(FramingError::SegmentTooLarge {
            kind: SegmentKind::ServiceElement,
            declared: u64::from(at_the_limit) + 1,
            limit: LIMITS.max_media_bytes,
        })
    );
}

#[test]
fn an_unfinished_fragment_never_holds_more_than_the_segment_limit() {
    let limit = u32::try_from(LIMITS.max_media_bytes).expect("small limit");
    let mut framer = IsoBmffFramer::new(LIMITS);
    let mut completed = Vec::new();
    // `moof` ровно на предел: тело копится, но не больше объявленного.
    framer
        .push(&box_header(limit, *b"moof"), &mut completed)
        .expect("at the limit");
    framer
        .push(&vec![0x11; LIMITS.max_media_bytes - 8], &mut completed)
        .expect("body");
    assert_eq!(framer.held_bytes(), LIMITS.max_media_bytes);
    // `moof` + `mdat` — один сегмент: второй бокс предел не обходит.
    assert_eq!(
        framer.push(&box_header(8, *b"mdat"), &mut completed),
        Err(FramingError::SegmentTooLarge {
            kind: SegmentKind::Media,
            declared: u64::from(limit) + 8,
            limit: LIMITS.max_media_bytes,
        })
    );
}
