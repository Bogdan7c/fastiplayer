//! Разметка времени WebM: настоящий VP9 + Opus из фикстур протокола и синтетика.

use crate::container::PushedContainer;
use crate::framing::{ContainerFramer, FramedUnit, FramingLimits};

use super::test_support::{
    block_body, ebml_element, ebml_unknown_size_element, ebml_unsigned, split_units, timed,
    timed_estimated, timed_open,
};
use super::{SegmentTimingContext, SegmentTimingError, TimingStructure};

/// VP9 (дорожка 1, `DefaultDuration` 40 мс) + Opus (дорожка 2, 20 мс), один кластер
/// с `Timecode` 0. Последний блок Opus — `BlockGroup` с меткой 401 и
/// `BlockDuration` 7, поэтому кластер кончается на 408 мс (проверено разбором
/// элементов независимо от этого кода).
const MUXED_WEBM: &[u8] =
    include_bytes!("../../../../browser-bridge-protocol/fixtures/media/muxed.webm");
/// Записанный поток «звук и видео в одном SourceBuffer» с перемоткой назад.
const SEEK_BACK_STREAM: &str = include_str!(
    "../../../../browser-bridge-protocol/fixtures/streams/muxed-av-single-source-buffer/stream.json"
);

const EBML_HEADER: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const DEFAULT_DURATION: u32 = 0x23_E383;
const CLUSTER: u32 = 0x1F43_B675;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const BLOCK_DURATION: u32 = 0x9B;

/// Флаги `SimpleBlock`: ключевой кадр, без лейсинга.
const KEYFRAME: u8 = 0x80;

/// Дорожка синтетического init: номер и `DefaultDuration` (нс).
struct SyntheticTrack {
    number: u64,
    default_duration_ns: Option<u64>,
}

/// Init WebM, как его хранит сканер: `Segment` неизвестного размера.
fn webm_initialization(timecode_scale: Option<u64>, tracks: &[SyntheticTrack]) -> Vec<u8> {
    let info_body = timecode_scale
        .map(|scale| ebml_unsigned(TIMECODE_SCALE, scale))
        .unwrap_or_default();
    let mut tracks_body = Vec::new();
    for track in tracks {
        let mut entry = ebml_unsigned(TRACK_NUMBER, track.number);
        if let Some(duration) = track.default_duration_ns {
            entry.extend(ebml_unsigned(DEFAULT_DURATION, duration));
        }
        tracks_body.extend(ebml_element(TRACK_ENTRY, &entry));
    }
    let mut segment_body = ebml_element(INFO, &info_body);
    segment_body.extend(ebml_element(TRACKS, &tracks_body));
    let mut initialization = ebml_element(EBML_HEADER, &[0x42, 0x82, 0x84, b'w', b'e', b'b', b'm']);
    initialization.extend(ebml_unknown_size_element(SEGMENT, &segment_body));
    initialization
}

/// Видео 40 мс/кадр и звук без `DefaultDuration`, шкала по умолчанию (1 мс).
fn default_context() -> SegmentTimingContext {
    let tracks = [
        SyntheticTrack {
            number: 1,
            default_duration_ns: Some(40_000_000),
        },
        SyntheticTrack {
            number: 2,
            default_duration_ns: None,
        },
    ];
    SegmentTimingContext::from_initialization(
        PushedContainer::WebM,
        &webm_initialization(None, &tracks),
    )
    .expect("synthetic init is valid")
}

/// Кластер с `Timecode` и готовыми детьми.
fn cluster(timecode: u64, children: &[Vec<u8>]) -> Vec<u8> {
    let mut body = ebml_unsigned(TIMECODE, timecode);
    for child in children {
        body.extend_from_slice(child);
    }
    ebml_element(CLUSTER, &body)
}

#[test]
fn recorded_webm_cluster_spans_its_blocks_including_block_duration() {
    let mut framer = ContainerFramer::new(PushedContainer::WebM, limits());
    let mut units = Vec::new();
    framer.push(MUXED_WEBM, &mut units).expect("valid WebM");
    let stream = split_units(units);
    let context = SegmentTimingContext::from_initialization(
        PushedContainer::WebM,
        &stream.initializations[0],
    )
    .expect("init with unknown-size Segment");
    assert_eq!(stream.media.len(), 1);
    assert_eq!(
        context.media_segment_timing(&stream.media[0]),
        Ok(timed(0, 408_000))
    );
}

#[test]
fn seek_back_in_recorded_stream_relabels_the_same_cluster_times() {
    // Записанный поток: страница положила байты [0, 3000), перемоталась назад
    // (`abort`) и положила весь файл заново. Диапазоны берутся из записи.
    let manifest: serde_json::Value = serde_json::from_str(SEEK_BACK_STREAM).expect("json");
    let ranges = manifest["expect"]["admitted"][0]["ranges"]
        .as_array()
        .expect("ranges")
        .iter()
        .map(|range| {
            let start = usize::try_from(range[0].as_u64().expect("start")).expect("small");
            let end = usize::try_from(range[1].as_u64().expect("end")).expect("small");
            start..end
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges.len(), 2, "one append before the seek, one after");

    let mut framer = ContainerFramer::new(PushedContainer::WebM, limits());
    let mut units: Vec<FramedUnit> = Vec::new();
    for range in ranges {
        framer.reset(); // перемотка страницы = abort()
        framer
            .push(&MUXED_WEBM[range], &mut units)
            .expect("valid WebM");
    }
    let stream = split_units(units);
    assert_eq!(
        stream.initializations.len(),
        2,
        "init before and after the seek"
    );
    let contexts: Vec<SegmentTimingContext> = stream
        .initializations
        .iter()
        .map(|init| {
            SegmentTimingContext::from_initialization(PushedContainer::WebM, init).expect("init")
        })
        .collect();
    assert_eq!(contexts[0], contexts[1]);
    assert_eq!(
        stream.media.len(),
        1,
        "the cluster arrives only after the seek"
    );
    assert_eq!(
        contexts[1].media_segment_timing(&stream.media[0]),
        Ok(timed(0, 408_000))
    );
}

#[test]
fn default_duration_and_cluster_timecode_set_the_span() {
    let segment = cluster(
        1_000,
        &[
            ebml_element(SIMPLE_BLOCK, &block_body(1, 0, KEYFRAME)),
            ebml_element(SIMPLE_BLOCK, &block_body(1, 40, 0)),
        ],
    );
    // 1000 мс + последний кадр 40 мс + `DefaultDuration` 40 мс.
    assert_eq!(
        default_context().media_segment_timing(&segment),
        Ok(timed(1_000_000, 1_080_000))
    );
}

#[test]
fn custom_timecode_scale_negative_relative_time_and_block_duration() {
    let tracks = [SyntheticTrack {
        number: 2,
        default_duration_ns: None,
    }];
    // Тик = 0,1 мс; кластер в 5000 тиков = 500 мс.
    let context = SegmentTimingContext::from_initialization(
        PushedContainer::WebM,
        &webm_initialization(Some(100_000), &tracks),
    )
    .expect("init");
    let mut group = ebml_element(BLOCK, &block_body(2, 30, 0));
    group.extend(ebml_unsigned(BLOCK_DURATION, 20));
    let segment = cluster(
        5_000,
        &[
            ebml_element(SIMPLE_BLOCK, &block_body(2, -10, KEYFRAME)),
            ebml_element(BLOCK_GROUP, &group),
        ],
    );
    // Начало: 5000 − 10 тиков = 499 мс; конец: 5000 + 30 + 20 тиков = 505 мс.
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(499_000, 505_000))
    );
}

#[test]
fn laced_block_lasts_default_duration_per_frame() {
    // Флаги 0x02 (Xiph-лейсинг), затем байт «кадров − 1» = 2 → 3 кадра × 40 мс.
    let mut laced = vec![0x81];
    laced.extend_from_slice(&0_i16.to_be_bytes());
    laced.extend_from_slice(&[0x82, 2, 0xAA, 0xAA]);
    let segment = cluster(0, &[ebml_element(SIMPLE_BLOCK, &laced)]);
    assert_eq!(
        default_context().media_segment_timing(&segment),
        Ok(timed(0, 120_000))
    );
}

#[test]
fn single_block_without_any_duration_has_an_open_end_and_empty_cluster_sits_at_its_timecode() {
    let audio_only = cluster(
        200,
        &[ebml_element(SIMPLE_BLOCK, &block_body(2, 0, KEYFRAME))],
    );
    // Длительность не записана, мерить не по чему: известно только начало кадра.
    assert_eq!(
        default_context().media_segment_timing(&audio_only),
        Ok(timed_open(200_000, 200_000))
    );
    let empty = cluster(300, &[]);
    assert_eq!(
        default_context().media_segment_timing(&empty),
        Ok(timed(300_000, 300_000))
    );
}

#[test]
fn last_block_without_duration_ends_one_frame_step_after_its_start() {
    // 4 к/с без `BlockDuration` и `DefaultDuration` (так пишут плееры страниц):
    // раньше кластер кончался на 750 мс, и до следующего (1000 мс) зияла дыра
    // в кадр — больше наибольшей дыры полки, подача вставала.
    let blocks: Vec<Vec<u8>> = [0_i16, 250, 500, 750]
        .into_iter()
        .map(|relative| ebml_element(SIMPLE_BLOCK, &block_body(2, relative, 0)))
        .collect();
    assert_eq!(
        default_context().media_segment_timing(&cluster(1_000, &blocks)),
        Ok(timed_estimated(1_000_000, 1_750_000, 2_000_000))
    );
    // Блоки не по порядку времени (порядок декодирования): шаг — между двумя
    // самыми поздними началами.
    let reordered: Vec<Vec<u8>> = [0_i16, 500, 250, 750]
        .into_iter()
        .map(|relative| ebml_element(SIMPLE_BLOCK, &block_body(2, relative, 0)))
        .collect();
    assert_eq!(
        default_context().media_segment_timing(&cluster(0, &reordered)),
        Ok(timed_estimated(0, 750_000, 1_000_000))
    );
    // У видео (дорожка 1) длительность есть и кончается позже оценки звука —
    // конец точный.
    let mixed = cluster(
        0,
        &[
            ebml_element(SIMPLE_BLOCK, &block_body(2, 0, KEYFRAME)),
            ebml_element(SIMPLE_BLOCK, &block_body(2, 20, 0)),
            ebml_element(SIMPLE_BLOCK, &block_body(1, 0, KEYFRAME)),
        ],
    );
    assert_eq!(
        default_context().media_segment_timing(&mixed),
        Ok(timed(0, 40_000))
    );
}

#[test]
fn hostile_huge_durations_and_scales_are_out_of_range_not_a_panic() {
    let tracks = [SyntheticTrack {
        number: 1,
        default_duration_ns: Some(u64::MAX),
    }];
    // `TimecodeScale` и `BlockDuration` по u64::MAX: их произведение больше i128.
    let huge_scale = SegmentTimingContext::from_initialization(
        PushedContainer::WebM,
        &webm_initialization(Some(u64::MAX), &tracks),
    )
    .expect("init itself is valid");
    let mut group = ebml_element(BLOCK, &block_body(1, 0, KEYFRAME));
    group.extend(ebml_unsigned(BLOCK_DURATION, u64::MAX));
    let block_duration = cluster(0, &[ebml_element(BLOCK_GROUP, &group)]);
    assert_eq!(
        huge_scale.media_segment_timing(&block_duration),
        Err(SegmentTimingError::TimeOutOfRange)
    );
    // Отрицательная метка при огромной шкале и огромный `Timecode` кластера.
    let negative = cluster(
        0,
        &[ebml_element(
            SIMPLE_BLOCK,
            &block_body(1, i16::MIN, KEYFRAME),
        )],
    );
    assert_eq!(
        huge_scale.media_segment_timing(&negative),
        Err(SegmentTimingError::TimeOutOfRange)
    );
    let far_cluster = cluster(
        u64::MAX,
        &[ebml_element(SIMPLE_BLOCK, &block_body(1, 0, 0))],
    );
    assert_eq!(
        huge_scale.media_segment_timing(&far_cluster),
        Err(SegmentTimingError::TimeOutOfRange)
    );
}

#[test]
fn unknown_size_cluster_is_read_to_the_end_of_the_segment() {
    let mut body = ebml_unsigned(TIMECODE, 0);
    body.extend(ebml_element(SIMPLE_BLOCK, &block_body(1, 0, KEYFRAME)));
    let segment = ebml_unknown_size_element(CLUSTER, &body);
    assert_eq!(
        default_context().media_segment_timing(&segment),
        Ok(timed(0, 40_000))
    );
}

#[test]
fn malformed_clusters_are_typed_errors() {
    let context = default_context();
    let foreign_track = cluster(
        0,
        &[ebml_element(SIMPLE_BLOCK, &block_body(7, 0, KEYFRAME))],
    );
    assert_eq!(
        context.media_segment_timing(&foreign_track),
        Err(SegmentTimingError::UnknownTrack { track: 7 })
    );

    let without_timecode = ebml_element(CLUSTER, &ebml_element(SIMPLE_BLOCK, &block_body(1, 0, 0)));
    assert_eq!(
        context.media_segment_timing(&without_timecode),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaClusterTimecode
        })
    );

    // Нулевой байт не начинает ни одного ID EBML.
    let garbage_child = cluster(0, &[vec![0x00, 0x81, 0x00]]);
    assert_eq!(
        context.media_segment_timing(&garbage_child),
        Err(SegmentTimingError::InvalidHeader {
            structure: TimingStructure::MatroskaCluster
        })
    );

    // Ребёнок объявляет 10 байтов тела, а до конца кластера их 2.
    let lying_child = cluster(0, &[vec![0xA3, 0x8A, 0x01, 0x02]]);
    assert_eq!(
        context.media_segment_timing(&lying_child),
        Err(SegmentTimingError::Truncated {
            structure: TimingStructure::MatroskaCluster
        })
    );

    let unknown_size_group = cluster(0, &[ebml_unknown_size_element(BLOCK_GROUP, &[])]);
    assert_eq!(
        context.media_segment_timing(&unknown_size_group),
        Err(SegmentTimingError::InvalidHeader {
            structure: TimingStructure::MatroskaCluster
        })
    );

    let truncated_block = cluster(0, &[ebml_element(SIMPLE_BLOCK, &[0x81, 0x00])]);
    assert_eq!(
        context.media_segment_timing(&truncated_block),
        Err(SegmentTimingError::Truncated {
            structure: TimingStructure::MatroskaBlock
        })
    );

    let not_a_cluster = ebml_element(TRACKS, &[]);
    assert_eq!(
        context.media_segment_timing(&not_a_cluster),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaCluster
        })
    );
}

#[test]
fn bad_webm_initializations_are_typed_errors() {
    let tracks = [SyntheticTrack {
        number: 1,
        default_duration_ns: None,
    }];
    assert_eq!(
        SegmentTimingContext::from_initialization(
            PushedContainer::WebM,
            &webm_initialization(Some(0), &tracks)
        ),
        Err(SegmentTimingError::ZeroTimecodeScale)
    );
    let duplicate = [
        SyntheticTrack {
            number: 1,
            default_duration_ns: None,
        },
        SyntheticTrack {
            number: 1,
            default_duration_ns: None,
        },
    ];
    assert_eq!(
        SegmentTimingContext::from_initialization(
            PushedContainer::WebM,
            &webm_initialization(None, &duplicate)
        ),
        Err(SegmentTimingError::DuplicateTrack { track: 1 })
    );
    let mut without_tracks = ebml_element(EBML_HEADER, &[]);
    without_tracks.extend(ebml_unknown_size_element(SEGMENT, &ebml_element(INFO, &[])));
    assert_eq!(
        SegmentTimingContext::from_initialization(PushedContainer::WebM, &without_tracks),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaTracks
        })
    );
    assert_eq!(
        SegmentTimingContext::from_initialization(PushedContainer::WebM, &cluster(0, &[])),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaSegment
        })
    );
    // Временная шкала из девяти байтов — не целое EBML.
    let mut oversized_scale = ebml_element(EBML_HEADER, &[]);
    let segment_body = ebml_element(INFO, &ebml_element(TIMECODE_SCALE, &[1; 9]));
    oversized_scale.extend(ebml_unknown_size_element(SEGMENT, &segment_body));
    assert_eq!(
        SegmentTimingContext::from_initialization(PushedContainer::WebM, &oversized_scale),
        Err(SegmentTimingError::InvalidHeader {
            structure: TimingStructure::MatroskaUnsignedInteger
        })
    );
}

/// Пределы сканера с запасом для фикстур.
fn limits() -> FramingLimits {
    FramingLimits {
        max_initialization_bytes: 1024 * 1024,
        max_media_bytes: 1024 * 1024,
    }
}
