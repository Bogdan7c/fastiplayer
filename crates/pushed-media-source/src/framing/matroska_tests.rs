//! Тесты сканера WebM на настоящем WebM VP9 + Opus из фикстур протокола.

use bytes::Bytes;

use super::{CLUSTER_ID, MatroskaFramer, TRACKS_ID};
use crate::framing::ebml::{
    EbmlElementSize, UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field, read_element_header,
};
use crate::framing::{FramedUnit, FramingError, FramingLimits, SegmentKind};
use crate::test_fixtures::pseudo_random_bytes;

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

/// Элемент: ID, 8-байтовый размер, тело из `body_len` байтов.
fn element(id: u32, body_len: usize) -> Vec<u8> {
    let mut bytes = element_header(id, body_len as u64);
    bytes.extend(std::iter::repeat_n(0x42, body_len));
    bytes
}

/// Заголовок элемента: ID и 8-байтовый размер, тела ещё нет.
fn element_header(id: u32, declared: u64) -> Vec<u8> {
    let mut bytes = encode_id(id);
    bytes.extend_from_slice(&encode_known_size_field(declared).expect("fits"));
    bytes
}

/// Отдельный список, а не константа сканера: убрать ID из сканера обязано ломать тест.
const LEGITIMATE_SERVICE_ELEMENTS: [u32; 7] = [
    0x114D_9B74, // SeekHead
    0x1C53_BB6B, // Cues
    0x1254_C367, // Tags
    0x1043_A770, // Chapters
    0x1941_A469, // Attachments
    0xEC,        // Void
    0xBF,        // CRC-32
];

/// ID, которых нет на уровне `Segment` и в кластере: длиной 1, 2, 3 и 4 байта;
/// `0xB6` (`ChapterAtom`) настоящий, но живёт глубже.
const GARBAGE_ELEMENT_IDS: [u32; 6] = [0x85, 0x4321, 0x21_F0_0D, 0x1F00_1234, 0xB6, 0x10AB_CDEF];

/// Размер меньше всех пределов: «правдоподобно большой» случайный заголовок.
const IMPLAUSIBLE_SIZE_UNDER_THE_CAP: u64 = 20 * 1024;
/// Страница объявляет элемент больше, чем поток имеет право держать.
const THREE_GIB: u64 = 3 * 1024 * 1024 * 1024;

/// Смещение первого кластера и первого ребёнка `Segment` в фикстуре.
fn fixture_offsets() -> (usize, usize) {
    let (_, children) = layout();
    let first_child = children.first().expect("children").start;
    let first_cluster = children
        .iter()
        .find(|child| child.id == CLUSTER_ID)
        .expect("cluster")
        .start;
    (first_child, first_cluster)
}

/// Кластер неизвестного размера из тела настоящего кластера плюс `extra_children`.
fn unknown_size_cluster(extra_children: &[u8]) -> Vec<u8> {
    let cluster = cluster_bytes();
    let header = read_element_header(cluster)
        .expect("valid")
        .expect("header");
    let mut bytes = encode_id(CLUSTER_ID);
    bytes.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    bytes.extend_from_slice(&cluster[header.header_len..]);
    bytes.extend_from_slice(extra_children);
    bytes
}

/// Сканер, прошедший `prefix` без ошибок.
fn framer_after(prefix: &[u8]) -> (MatroskaFramer, Vec<FramedUnit>) {
    let mut framer = MatroskaFramer::new(LIMITS);
    let mut completed = Vec::new();
    framer.push(prefix, &mut completed).expect("valid prefix");
    (framer, completed)
}

/// Все положения в потоке, где сканер читает заголовок элемента, кроме самого начала:
/// внутри init, после init, после кластера, внутри кластера неизвестного размера.
fn streams_before_an_element_header() -> [Vec<u8>; 4] {
    let (first_child, first_cluster) = fixture_offsets();
    let mut after_init = MUXED_WEBM[..first_cluster].to_vec();
    // Первый кластер выдаёт init, поэтому «после init» — с пустым известным кластером.
    after_init.extend(element(CLUSTER_ID, 0));
    let mut after_cluster = MUXED_WEBM[..first_cluster].to_vec();
    after_cluster.extend_from_slice(cluster_bytes());
    let mut inside_open_cluster = MUXED_WEBM[..first_cluster].to_vec();
    inside_open_cluster.extend(unknown_size_cluster(&[]));
    [
        MUXED_WEBM[..first_child].to_vec(),
        after_init,
        after_cluster,
        inside_open_cluster,
    ]
}

#[test]
fn every_service_element_is_skipped_in_any_chunking() {
    let (first_child, first_cluster) = fixture_offsets();
    for service_id in LEGITIMATE_SERVICE_ELEMENTS {
        // Сразу за заголовком `Segment`, перед первым кластером, между кластерами, в конце.
        let mut stream = MUXED_WEBM[..first_child].to_vec();
        stream.extend(element(service_id, 4));
        stream.extend_from_slice(&MUXED_WEBM[first_child..first_cluster]);
        stream.extend(element(service_id, 300));
        stream.extend_from_slice(cluster_bytes());
        stream.extend(element(service_id, 0));
        stream.extend_from_slice(cluster_bytes());
        stream.extend(element(service_id, 7));
        for chunk_len in [1, 17, 1000, stream.len()] {
            let (units, framer) = frame(&stream, chunk_len);
            assert_eq!(framer.held_bytes(), 0, "{service_id:X} chunk {chunk_len}");
            let media: Vec<&FramedUnit> = units
                .iter()
                .filter(|unit| matches!(unit, FramedUnit::Media(_)))
                .collect();
            let whole_cluster = FramedUnit::Media(Bytes::from_static(cluster_bytes()));
            assert_eq!(units.len(), 3, "{service_id:X} chunk {chunk_len}");
            assert_eq!(media, [&whole_cluster, &whole_cluster]);
        }
    }
}

#[test]
fn every_legitimate_child_of_an_unknown_size_cluster_is_kept() {
    let (_, first_cluster) = fixture_offsets();
    let mut children = Vec::new();
    for child_id in [0xA0, 0xA3, 0xA7, 0xAB, 0xAF, 0x5854, 0xEC, 0xBF] {
        children.extend(element(child_id, 5));
    }
    let mut stream = MUXED_WEBM[..first_cluster].to_vec();
    stream.extend(unknown_size_cluster(&children));
    stream.extend_from_slice(cluster_bytes());
    let (units, _) = frame(&stream, 23);
    assert_eq!(units.len(), 3, "the open cluster ends at the next cluster");
    let FramedUnit::Media(open_cluster) = &units[1] else {
        panic!("media");
    };
    assert!(
        open_cluster.ends_with(&children),
        "every child reaches the demuxer"
    );
}

#[test]
fn a_stray_segment_header_does_not_swallow_the_stream_behind_it() {
    let (first_child, first_cluster) = fixture_offsets();
    // Объявленное тело больше потока: пропуск «тела» проглотил бы всё дальше.
    let stray_segment = element_header(0x1853_8067, THREE_GIB);
    let mut stream = MUXED_WEBM[..first_child].to_vec();
    stream.extend_from_slice(&stray_segment);
    stream.extend_from_slice(&MUXED_WEBM[first_child..first_cluster]);
    stream.extend_from_slice(cluster_bytes());
    stream.extend_from_slice(&stray_segment);
    stream.extend_from_slice(cluster_bytes());
    let (units, framer) = frame(&stream, 29);
    assert_eq!(units.len(), 3, "init and both clusters");
    assert_eq!(framer.held_bytes(), 0);
}

#[test]
fn garbage_on_the_first_bytes_is_refused() {
    let mut completed = Vec::new();
    for seed in 0..2_000 {
        let garbage = pseudo_random_bytes(seed, 64 * 1024);
        assert!(
            MatroskaFramer::new(LIMITS)
                .push(&garbage, &mut completed)
                .is_err(),
            "seed {seed} was swallowed"
        );
    }
    assert!(completed.is_empty());
}

#[test]
fn a_garbage_element_is_refused_at_its_header_wherever_it_lands() {
    for stream in streams_before_an_element_header() {
        for id in GARBAGE_ELEMENT_IDS {
            let (mut framer, mut completed) = framer_after(&stream);
            let units_before_garbage = completed.len();
            assert_eq!(
                framer.push(
                    &element_header(id, IMPLAUSIBLE_SIZE_UNDER_THE_CAP),
                    &mut completed
                ),
                Err(FramingError::UnexpectedEbmlElement { id }),
                "{id:X} after {} bytes",
                stream.len()
            );
            assert_eq!(completed.len(), units_before_garbage, "nothing is released");
        }
    }
}

#[test]
fn a_garbage_header_split_across_chunks_is_refused_on_its_last_byte() {
    let id = 0x1F00_1234;
    let header = element_header(id, IMPLAUSIBLE_SIZE_UNDER_THE_CAP);
    let [_, _, after_cluster, _] = streams_before_an_element_header();
    let (mut framer, mut completed) = framer_after(&after_cluster);
    let (last_byte, head) = header.split_last().expect("header");
    for byte in head {
        framer
            .push(std::slice::from_ref(byte), &mut completed)
            .expect("the header is not whole yet");
    }
    assert_eq!(
        framer.push(std::slice::from_ref(last_byte), &mut completed),
        Err(FramingError::UnexpectedEbmlElement { id })
    );
}

/// Доля случайных кусков, которые могут остаться без ответа: их начало случайно читается
/// как настоящий элемент (`Void`, `SimpleBlock`) с телом длиннее куска. Такой кусок ждёт
/// и настоящий браузер; главное — у WebM это редкость, а не правило, как было.
const RANDOM_GARBAGE_WAIT_SHARE_PERCENT: usize = 1;

#[test]
fn random_bytes_are_refused_after_valid_webm_except_for_a_rare_real_looking_header() {
    const SEEDS: u64 = 2_000;
    let streams = streams_before_an_element_header();
    for stream in &streams[1..] {
        let swallowed = (0..SEEDS)
            .filter(|seed| {
                let (mut framer, mut completed) = framer_after(stream);
                framer
                    .push(&pseudo_random_bytes(*seed, 64 * 1024), &mut completed)
                    .is_ok()
            })
            .count();
        assert!(
            swallowed * 100
                <= usize::try_from(SEEDS).expect("small") * RANDOM_GARBAGE_WAIT_SHARE_PERCENT,
            "{swallowed} of {SEEDS} random chunks after {} bytes were swallowed",
            stream.len()
        );
    }
}

#[test]
fn a_declared_three_gigabytes_are_refused_by_the_header_for_every_kind_of_element() {
    let (_, first_cluster) = fixture_offsets();
    let [_, after_init, after_cluster, inside_open_cluster] = streams_before_an_element_header();
    let refused = [
        (&after_cluster, 0xEC, SegmentKind::ServiceElement), // Void
        (&after_init, 0x1C53_BB6B, SegmentKind::ServiceElement), // Cues
        (&after_cluster, CLUSTER_ID, SegmentKind::Media),
    ];
    for (stream, id, kind) in refused {
        let header = element_header(id, THREE_GIB);
        let (mut framer, mut completed) = framer_after(stream);
        assert_eq!(
            framer.push(&header, &mut completed),
            Err(FramingError::SegmentTooLarge {
                kind,
                declared: THREE_GIB + header.len() as u64,
                limit: LIMITS.max_media_bytes,
            })
        );
    }
    // Внутри init и внутри кластера неизвестного размера — тот же отказ по заголовку.
    let (mut framer, mut completed) = framer_after(&MUXED_WEBM[..first_cluster]);
    let void = element_header(0xEC, THREE_GIB);
    assert!(matches!(
        framer.push(&void, &mut completed),
        Err(FramingError::SegmentTooLarge { .. })
    ));
    let (mut framer, mut completed) = framer_after(&inside_open_cluster);
    let block = element_header(0xA3, THREE_GIB);
    assert!(matches!(
        framer.push(&block, &mut completed),
        Err(FramingError::SegmentTooLarge { .. })
    ));
    assert!(framer.held_bytes() < LIMITS.max_media_bytes);
}

#[test]
fn a_service_element_up_to_the_limit_is_skipped_without_holding_its_bytes() {
    let [_, _, after_cluster, _] = streams_before_an_element_header();
    let header = element_header(0xEC, 0);
    let body_len = LIMITS.max_media_bytes - header.len();
    let (mut framer, mut completed) = framer_after(&after_cluster);
    framer
        .push(&element_header(0xEC, body_len as u64), &mut completed)
        .expect("under the cap");
    // Страница капает настоящие байты: память не растёт ни на байт.
    for _ in 0..body_len / 1024 {
        framer.push(&[0x5A; 1024], &mut completed).expect("body");
        assert_eq!(framer.held_bytes(), 0);
    }
    framer
        .push(&vec![0x5A; body_len % 1024], &mut completed)
        .expect("end");
    let units_before_cluster = completed.len();
    framer
        .push(cluster_bytes(), &mut completed)
        .expect("the stream goes on after the element");
    assert_eq!(completed.len(), units_before_cluster + 1);
}

#[test]
fn an_unknown_size_cluster_fed_forever_stops_at_the_segment_limit() {
    let [_, _, _, inside_open_cluster] = streams_before_an_element_header();
    let (mut framer, mut completed) = framer_after(&inside_open_cluster);
    let block = element(0xA3, 4096);
    let mut outcome = Ok(());
    let mut pushed = 0;
    while outcome.is_ok() && pushed <= 2 * LIMITS.max_media_bytes {
        outcome = framer.push(&block, &mut completed);
        pushed += block.len();
    }
    assert!(
        matches!(outcome, Err(FramingError::SegmentTooLarge { .. })),
        "{outcome:?}"
    );
    assert!(framer.held_bytes() <= LIMITS.max_media_bytes + block.len());
}
