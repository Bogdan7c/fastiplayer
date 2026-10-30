//! Разметка времени fMP4: настоящие фикстуры и синтетические случаи.

use crate::PushedStreamFailureClass;
use crate::container::PushedContainer;
use crate::error::PushedStreamError;
use crate::framing::FramingError;
use crate::lane::LaneId;
use crate::time::TimestampOffset;

use super::test_support::{
    SyntheticIsoTrack, frame_stream, full_box, iso_box, iso_initialization, iso_media_segment,
    timed, track_fragment, track_run,
};
use super::{
    MediaTime, MediaTimeSpan, SegmentTiming, SegmentTimingContext, SegmentTimingError,
    TimingStructure,
};

/// H.264 64×64, 25 к/с, 4 фрагмента по 5 кадров, шкала 12800, длительность из `tfhd`.
const VIDEO_H264_64: &[u8] = include_bytes!("../../../fixtures/video-h264-64.mp4");
/// VP9 96×96 с той же нарезкой.
const VIDEO_VP9_96: &[u8] = include_bytes!("../../../fixtures/video-vp9-96.mp4");
/// AAC 48 кГц: 3 фрагмента по 10 кадров × 1024, последний — 9 кадров со своими длительностями.
const AUDIO_AAC: &[u8] = include_bytes!("../../../fixtures/audio-aac.mp4");

/// Флаги `trun`: своя длительность у каждого кадра.
const RUN_DURATIONS: u32 = 0x100;
/// Флаги `trun`: своё смещение композиции у каждого кадра.
const RUN_COMPOSITION_OFFSETS: u32 = 0x800;

/// Видео 25 к/с, шкала 12800: кадр = 512 тиков = 40 мс; `trex` = 512.
const VIDEO_TRACK: SyntheticIsoTrack = SyntheticIsoTrack {
    track_id: 1,
    timescale: 12_800,
    extends_default_duration: Some(512),
};
/// Звук 48 кГц: кадр AAC = 1024 тика; `trex` = 1024.
const AUDIO_TRACK: SyntheticIsoTrack = SyntheticIsoTrack {
    track_id: 2,
    timescale: 48_000,
    extends_default_duration: Some(1024),
};

/// Все медиасегменты фикстуры, размеченные контекстом её init.
fn fixture_timings(fixture: &[u8]) -> Vec<SegmentTiming> {
    let stream = frame_stream(PushedContainer::FragmentedMp4, fixture);
    assert_eq!(stream.initializations.len(), 1);
    let context = SegmentTimingContext::from_initialization(
        PushedContainer::FragmentedMp4,
        &stream.initializations[0],
    )
    .expect("fixture init is valid");
    stream
        .media
        .iter()
        .map(|segment| {
            context
                .media_segment_timing(segment)
                .expect("valid fragment")
        })
        .collect()
}

/// Контекст синтетического init.
fn synthetic_context(tracks: &[SyntheticIsoTrack]) -> SegmentTimingContext {
    SegmentTimingContext::from_initialization(
        PushedContainer::FragmentedMp4,
        &iso_initialization(tracks),
    )
    .expect("synthetic init is valid")
}

#[test]
fn video_fragments_cover_consecutive_200_ms_windows() {
    for fixture in [VIDEO_H264_64, VIDEO_VP9_96] {
        let expected: Vec<SegmentTiming> = (0..4)
            .map(|index| timed(index * 200_000, (index + 1) * 200_000))
            .collect();
        assert_eq!(fixture_timings(fixture), expected);
    }
}

#[test]
fn audio_fragments_use_48_khz_timescale_and_per_sample_durations() {
    // 10240 тиков при 48 кГц = 213 333,(3) мкс; границы округляются вниз одинаково,
    // поэтому соседние фрагменты стыкуются без дыр. Последний фрагмент — 8×1024 + 512
    // тиков из записей `trun` (если бы брались значения по умолчанию, было бы 832 000).
    assert_eq!(
        fixture_timings(AUDIO_AAC),
        vec![
            timed(0, 213_333),
            timed(213_333, 426_666),
            timed(426_666, 640_000),
            timed(640_000, 821_333),
        ]
    );
}

#[test]
fn muxed_fragment_spans_the_union_of_its_tracks_and_falls_back_to_trex() {
    let context = synthetic_context(&[VIDEO_TRACK, AUDIO_TRACK]);
    // Видео: tfdt 0,2 с, 5 кадров без длительностей в `tfhd`/`trun` → `trex` 512.
    // Звук: tfdt 9600 (0,2 с), 10 кадров по 1024 → конец 19840 тиков = 413 333 мкс.
    let segment = iso_media_segment(&[
        track_fragment(1, None, 2_560, &[track_run(0, 0, 5, &[])]),
        track_fragment(2, None, 9_600, &[track_run(0, 0, 10, &[])]),
    ]);
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(200_000, 413_333))
    );
}

#[test]
fn tfhd_default_duration_overrides_trex() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let segment = iso_media_segment(&[track_fragment(
        1,
        Some(1_024),
        0,
        &[track_run(0, 0, 5, &[])],
    )]);
    // 5 × 1024 тиков = 400 мс, а не 5 × 512 = 200 мс из `trex`.
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(0, 400_000))
    );
}

#[test]
fn consecutive_runs_continue_decode_time() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let runs = [track_run(0, 0, 2, &[]), track_run(0, 0, 3, &[])];
    let segment = iso_media_segment(&[track_fragment(1, None, 0, &runs)]);
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(0, 200_000))
    );
}

#[test]
fn composition_offsets_give_presentation_time_not_decode_time() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    // dts 0, 512, 1024; cto 512, 1024, 0 → pts 512, 1536, 1024; концы 1024, 2048, 1536.
    // Время показа [512, 2048) тиков = [40, 160) мс; по времени декодирования было бы [0, 120).
    let entries = [512, 512, 512, 1_024, 512, 0];
    let run = track_run(0, RUN_DURATIONS | RUN_COMPOSITION_OFFSETS, 3, &entries);
    let segment = iso_media_segment(&[track_fragment(1, None, 0, &[run])]);
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(40_000, 160_000))
    );
}

#[test]
fn signed_composition_offset_of_version_1_moves_time_below_zero() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let minus_one_frame = (-512_i32).cast_unsigned();
    let run = track_run(
        1,
        RUN_DURATIONS | RUN_COMPOSITION_OFFSETS,
        1,
        &[512, minus_one_frame],
    );
    let segment = iso_media_segment(&[track_fragment(1, None, 0, &[run])]);
    let timing = context.media_segment_timing(&segment).expect("valid");
    assert_eq!(timing, timed(-40_000, 0));
    let SegmentTiming::Timed(span) = timing else {
        unreachable!("fMP4 is timed")
    };
    // Смещение полосы складывается в тех же микросекундах.
    assert_eq!(
        span.shifted_by(TimestampOffset::from_micros(40_000))
            .map(super::TimedSpan::span),
        MediaTimeSpan::new(MediaTime::from_micros(0), MediaTime::from_micros(40_000))
    );
}

#[test]
fn fragment_without_samples_is_an_empty_span_at_its_decode_time() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let segment = iso_media_segment(&[track_fragment(1, None, 5_120, &[track_run(0, 0, 0, &[])])]);
    let timing = context.media_segment_timing(&segment).expect("valid");
    assert_eq!(timing, timed(400_000, 400_000));
    let SegmentTiming::Timed(span) = timing else {
        unreachable!("fMP4 is timed")
    };
    assert!(span.span().is_empty());
}

#[test]
fn huge_sample_count_without_entries_is_computed_not_iterated() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let segment = iso_media_segment(&[track_fragment(
        1,
        None,
        0,
        &[track_run(0, 0, u32::MAX, &[])],
    )]);
    // u32::MAX кадров × 40 мс — одна формула, без перебора миллиардов кадров.
    let expected_end = i64::from(u32::MAX) * 40_000;
    assert_eq!(
        context.media_segment_timing(&segment),
        Ok(timed(0, expected_end))
    );
}

#[test]
fn sample_count_beyond_run_body_is_rejected() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let short_run = track_run(0, RUN_DURATIONS, 5, &[512, 512]);
    let segment = iso_media_segment(&[track_fragment(1, None, 0, &[short_run])]);
    assert_eq!(
        context.media_segment_timing(&segment),
        Err(SegmentTimingError::SampleCountExceedsBox {
            sample_count: 5,
            available_bytes: 8
        })
    );
    let absurd_run = track_run(0, RUN_DURATIONS, u32::MAX, &[512]);
    let segment = iso_media_segment(&[track_fragment(1, None, 0, &[absurd_run])]);
    assert!(matches!(
        context.media_segment_timing(&segment),
        Err(SegmentTimingError::SampleCountExceedsBox {
            sample_count: u32::MAX,
            ..
        })
    ));
}

#[test]
fn unknown_track_missing_or_truncated_decode_time_are_typed_errors() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    let foreign = iso_media_segment(&[track_fragment(9, None, 0, &[track_run(0, 0, 1, &[])])]);
    assert_eq!(
        context.media_segment_timing(&foreign),
        Err(SegmentTimingError::UnknownTrack { track: 9 })
    );

    let header = full_box(b"tfhd", 0, 0, &1_u32.to_be_bytes());
    let without_decode_time = iso_media_segment(&[iso_box(b"traf", &header)]);
    assert_eq!(
        context.media_segment_timing(&without_decode_time),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::IsoDecodeTime
        })
    );

    // `tfdt` версии 1 обещает 8 байтов времени, а несёт 4.
    let mut truncated_body = header.clone();
    truncated_body.extend(full_box(b"tfdt", 1, 0, &[0; 4]));
    let truncated = iso_media_segment(&[iso_box(b"traf", &truncated_body)]);
    assert_eq!(
        context.media_segment_timing(&truncated),
        Err(SegmentTimingError::Truncated {
            structure: TimingStructure::IsoDecodeTime
        })
    );

    let mut future_version_body = header;
    future_version_body.extend(full_box(b"tfdt", 2, 0, &[0; 8]));
    let future_version = iso_media_segment(&[iso_box(b"traf", &future_version_body)]);
    assert_eq!(
        context.media_segment_timing(&future_version),
        Err(SegmentTimingError::UnsupportedVersion {
            structure: TimingStructure::IsoDecodeTime,
            version: 2
        })
    );
}

#[test]
fn segment_without_fragment_header_or_track_fragments_is_rejected() {
    let context = synthetic_context(&[VIDEO_TRACK]);
    assert_eq!(
        context.media_segment_timing(&iso_box(b"mdat", &[1, 2, 3])),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::IsoMovieFragment
        })
    );
    assert_eq!(
        context.media_segment_timing(&iso_media_segment(&[])),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::IsoTrackFragment
        })
    );
    // Бокс внутри `moof` объявляет больше байтов, чем есть.
    let mut lying_fragment = iso_box(b"moof", &iso_box(b"traf", &[0; 16]));
    lying_fragment[11] = 0xFF;
    assert_eq!(
        context.media_segment_timing(&lying_fragment),
        Err(SegmentTimingError::Truncated {
            structure: TimingStructure::IsoMovieFragment
        })
    );
}

#[test]
fn bad_initializations_are_typed_errors() {
    let zero_timescale = SyntheticIsoTrack {
        timescale: 0,
        ..VIDEO_TRACK
    };
    assert_eq!(
        SegmentTimingContext::from_initialization(
            PushedContainer::FragmentedMp4,
            &iso_initialization(&[zero_timescale])
        ),
        Err(SegmentTimingError::ZeroTimescale { track: 1 })
    );
    assert_eq!(
        SegmentTimingContext::from_initialization(
            PushedContainer::FragmentedMp4,
            &iso_initialization(&[VIDEO_TRACK, VIDEO_TRACK])
        ),
        Err(SegmentTimingError::DuplicateTrack { track: 1 })
    );
    assert_eq!(
        SegmentTimingContext::from_initialization(
            PushedContainer::FragmentedMp4,
            &iso_box(b"ftyp", b"isom")
        ),
        Err(SegmentTimingError::Missing {
            structure: TimingStructure::IsoMovie
        })
    );
    let initialization = iso_initialization(&[VIDEO_TRACK]);
    assert!(matches!(
        SegmentTimingContext::from_initialization(
            PushedContainer::FragmentedMp4,
            &initialization[..initialization.len() - 3]
        ),
        Err(SegmentTimingError::Truncated { .. })
    ));
}

#[test]
fn transport_stream_is_untimed_without_reading_bytes() {
    let context = SegmentTimingContext::from_initialization(PushedContainer::MpegTs, &[])
        .expect("no init for TS");
    assert_eq!(context, SegmentTimingContext::Untimed);
    assert_eq!(
        context.media_segment_timing(&[0x47; 188]),
        Ok(SegmentTiming::Untimed)
    );
}

#[test]
fn timing_errors_are_unparsable_media_for_the_stream() {
    let framing_error = FramingError::from(SegmentTimingError::UnknownTrack { track: 9 });
    let stream_error = PushedStreamError::Framing {
        lane: LaneId::new(0),
        source: framing_error,
    };
    assert_eq!(
        stream_error.failure_class(),
        PushedStreamFailureClass::UnparsableMedia
    );
}

#[test]
fn span_rejects_reversed_bounds_and_offset_overflow() {
    let later = MediaTime::from_micros(10);
    let earlier = MediaTime::from_micros(-10);
    assert_eq!(MediaTimeSpan::new(later, earlier), None);
    let span = MediaTimeSpan::new(earlier, later).expect("ordered");
    assert_eq!(span.start().as_micros(), -10);
    assert_eq!(span.end().as_micros(), 10);
    assert_eq!(
        span.union(MediaTimeSpan::empty_at(MediaTime::from_micros(50))),
        MediaTimeSpan::new(earlier, MediaTime::from_micros(50)).expect("ordered")
    );
    assert_eq!(
        span.shifted_by(TimestampOffset::from_micros(i64::MAX)),
        None
    );
    assert_eq!(
        span.shifted_by(TimestampOffset::from_micros(-100)),
        MediaTimeSpan::new(MediaTime::from_micros(-110), MediaTime::from_micros(-90))
    );
}
