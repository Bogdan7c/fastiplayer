//! Общие помощники тестов разметки времени: нарезка фикстур настоящим
//! сканером и сборка синтетических боксов/элементов байт за байтом.

use bytes::Bytes;

use crate::container::PushedContainer;
use crate::framing::ebml::{UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field};
use crate::framing::{ContainerFramer, FramedUnit, FramingLimits};

use super::{MediaTime, MediaTimeSpan, SegmentEnd, SegmentTiming, TimedSpan};

/// Пределы с запасом для крошечных фикстур.
const TEST_LIMITS: FramingLimits = FramingLimits {
    max_initialization_bytes: 1024 * 1024,
    max_media_bytes: 1024 * 1024,
};

/// Сегменты потока в порядке выдачи сканером.
pub(super) struct FramedStream {
    /// Init-сегменты.
    pub(super) initializations: Vec<Bytes>,
    /// Медиасегменты.
    pub(super) media: Vec<Bytes>,
}

/// Режет поток настоящим сканером полосы — ровно так, как сегменты увидит полка.
pub(super) fn frame_stream(container: PushedContainer, stream: &[u8]) -> FramedStream {
    let mut framer = ContainerFramer::new(container, TEST_LIMITS);
    let mut units = Vec::new();
    framer
        .push(stream, &mut units)
        .expect("fixture frames cleanly");
    assert_eq!(
        framer.finish(&mut units),
        0,
        "fixture leaves no partial segment"
    );
    split_units(units)
}

/// Раскладывает выданные сегменты на init и медиа.
pub(super) fn split_units(units: Vec<FramedUnit>) -> FramedStream {
    let mut stream = FramedStream {
        initializations: Vec::new(),
        media: Vec::new(),
    };
    for unit in units {
        match unit {
            FramedUnit::Initialization(bytes) => stream.initializations.push(bytes),
            FramedUnit::Media(bytes) => stream.media.push(bytes),
        }
    }
    stream
}

/// Отрезок `[start_us, end_us)`.
fn span_us(start_us: i64, end_us: i64) -> MediaTimeSpan {
    MediaTimeSpan::new(
        MediaTime::from_micros(start_us),
        MediaTime::from_micros(end_us),
    )
    .expect("ordered bounds")
}

/// Ожидаемый размеченный отрезок `[start_us, end_us)` с точным концом.
pub(super) fn timed(start_us: i64, end_us: i64) -> SegmentTiming {
    SegmentTiming::Timed(TimedSpan::exact(span_us(start_us, end_us)))
}

/// Ожидаемый отрезок с оценённым концом: точно известно до `certain_end_us`.
pub(super) fn timed_estimated(start_us: i64, certain_end_us: i64, end_us: i64) -> SegmentTiming {
    SegmentTiming::Timed(TimedSpan::new(
        span_us(start_us, end_us),
        SegmentEnd::Estimated {
            certain_end: MediaTime::from_micros(certain_end_us),
        },
    ))
}

/// Ожидаемый отрезок с открытым концом (точно известно до `end_us`).
pub(super) fn timed_open(start_us: i64, end_us: i64) -> SegmentTiming {
    SegmentTiming::Timed(TimedSpan::new(
        span_us(start_us, end_us),
        SegmentEnd::Open {
            certain_end: MediaTime::from_micros(end_us),
        },
    ))
}

/// Бокс ISO BMFF: 32-битный размер, тип, тело.
pub(super) fn iso_box(box_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let total = u32::try_from(8 + body.len()).expect("small test box");
    let mut bytes = total.to_be_bytes().to_vec();
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(body);
    bytes
}

/// Полный бокс: версия, 24 бита флагов, тело.
pub(super) fn full_box(box_type: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version];
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    iso_box(box_type, &body)
}

/// Дорожка синтетического init fMP4.
#[derive(Debug, Clone, Copy)]
pub(super) struct SyntheticIsoTrack {
    /// `track_ID`.
    pub(super) track_id: u32,
    /// `mdhd.timescale`.
    pub(super) timescale: u32,
    /// `trex.default_sample_duration`; `None` — `trex` не пишется.
    pub(super) extends_default_duration: Option<u32>,
}

/// Init fMP4 (`ftyp` + `moov` с `trak`-ами версии 0 и `mvex`).
pub(super) fn iso_initialization(tracks: &[SyntheticIsoTrack]) -> Vec<u8> {
    let mut movie_body = Vec::new();
    let mut extends_body = Vec::new();
    for track in tracks {
        let mut track_header = vec![0; 8]; // creation_time + modification_time
        track_header.extend_from_slice(&track.track_id.to_be_bytes());
        track_header.extend_from_slice(&[0; 4]); // reserved
        let mut media_header = vec![0; 8];
        media_header.extend_from_slice(&track.timescale.to_be_bytes());
        media_header.extend_from_slice(&[0; 8]); // duration + language
        let media = iso_box(b"mdia", &full_box(b"mdhd", 0, 0, &media_header));
        let mut track_body = full_box(b"tkhd", 0, 0, &track_header);
        track_body.extend(media);
        movie_body.extend(iso_box(b"trak", &track_body));
        if let Some(default_duration) = track.extends_default_duration {
            let mut extends = track.track_id.to_be_bytes().to_vec();
            extends.extend_from_slice(&1_u32.to_be_bytes()); // sample description index
            extends.extend_from_slice(&default_duration.to_be_bytes());
            extends.extend_from_slice(&[0; 8]); // default size + flags
            extends_body.extend(full_box(b"trex", 0, 0, &extends));
        }
    }
    movie_body.extend(iso_box(b"mvex", &extends_body));
    let mut initialization = iso_box(b"ftyp", b"isom\0\0\0\0");
    initialization.extend(iso_box(b"moov", &movie_body));
    initialization
}

/// Фрагмент одной дорожки (`traf`): `tfhd`, `tfdt` версии 1 и готовые `trun`.
pub(super) fn track_fragment(
    track_id: u32,
    header_default_duration: Option<u32>,
    base_decode_time: u64,
    runs: &[Vec<u8>],
) -> Vec<u8> {
    let mut header = track_id.to_be_bytes().to_vec();
    let mut header_flags = 0x02_0000; // default-base-is-moof
    if let Some(duration) = header_default_duration {
        header_flags |= 0x08;
        header.extend_from_slice(&duration.to_be_bytes());
    }
    let mut body = full_box(b"tfhd", 0, header_flags, &header);
    body.extend(full_box(b"tfdt", 1, 0, &base_decode_time.to_be_bytes()));
    for run in runs {
        body.extend_from_slice(run);
    }
    iso_box(b"traf", &body)
}

/// `trun` с указанными флагами и уже закодированными записями кадров.
pub(super) fn track_run(version: u8, flags: u32, sample_count: u32, entries: &[u32]) -> Vec<u8> {
    let mut payload = sample_count.to_be_bytes().to_vec();
    for field in entries {
        payload.extend_from_slice(&field.to_be_bytes());
    }
    full_box(b"trun", version, flags, &payload)
}

/// Медиасегмент fMP4: `moof` с данными `traf` и пустой `mdat`.
pub(super) fn iso_media_segment(track_fragments: &[Vec<u8>]) -> Vec<u8> {
    let mut fragment_body = full_box(b"mfhd", 0, 0, &1_u32.to_be_bytes());
    for track_fragment in track_fragments {
        fragment_body.extend_from_slice(track_fragment);
    }
    let mut segment = iso_box(b"moof", &fragment_body);
    segment.extend(iso_box(b"mdat", &[]));
    segment
}

/// Элемент EBML известного размера (8-байтовое поле размера).
pub(super) fn ebml_element(id: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = encode_id(id);
    bytes.extend_from_slice(&encode_known_size_field(body.len() as u64).expect("small"));
    bytes.extend_from_slice(body);
    bytes
}

/// Элемент EBML неизвестного размера.
pub(super) fn ebml_unknown_size_element(id: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = encode_id(id);
    bytes.extend_from_slice(&UNKNOWN_SIZE_FIELD);
    bytes.extend_from_slice(body);
    bytes
}

/// Беззнаковое целое EBML минимальной длины (0 — пустое тело).
pub(super) fn ebml_unsigned(id: u32, value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first_significant = bytes.iter().position(|byte| *byte != 0).unwrap_or(8);
    ebml_element(id, &bytes[first_significant..])
}

/// Тело `SimpleBlock`/`Block`: однобайтовый номер дорожки, метка, флаги, кадр.
pub(super) fn block_body(track_number: u8, relative_timecode: i16, flags: u8) -> Vec<u8> {
    let mut body = vec![0x80 | track_number];
    body.extend_from_slice(&relative_timecode.to_be_bytes());
    body.push(flags);
    body.extend_from_slice(&[0xAA; 3]); // байты кадра времени не касаются
    body
}
