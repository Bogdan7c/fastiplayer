//! Тесты разбора документов DASH и сведения ошибок транспорта.

use std::num::NonZeroUsize;

use subtitle_core::SubtitleContent;
use web_media_dash::{
    DashTextDocument, DashTextDocumentFormat, DashTextError, DashTextFetchedSegment,
};

use super::decode::decode_documents;
use crate::stream_subtitles::error::{StreamSubtitleError, StreamSubtitleFailureKind};

const LIMIT: NonZeroUsize = NonZeroUsize::new(64 * 1024).unwrap();

fn ttml(begin: &str, end: &str, text: &str) -> DashTextDocument {
    DashTextDocument {
        format: DashTextDocumentFormat::Ttml,
        bytes: format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div>
               <p begin="{begin}" end="{end}">{text}</p></div></body></tt>"#
        )
        .into_bytes(),
        sample_interval: None,
    }
}

fn plain_text(event: &subtitle_core::SubtitleEvent) -> String {
    match event.content() {
        SubtitleContent::Text(cue) => cue.text.plain_text(),
        other => panic!("unexpected content {other:?}"),
    }
}

#[test]
fn ttml_events_are_shifted_to_the_player_timeline_by_the_negative_and_positive_shift() {
    let fetched = DashTextFetchedSegment {
        documents: vec![ttml("00:00:10.000", "00:00:12.500", "Hello")],
        player_shift_micros: -8_000_000,
    };
    let events = decode_documents(&fetched, LIMIT).expect("decodes");
    assert_eq!(events.len(), 1);
    assert_eq!(plain_text(&events[0]), "Hello");
    assert_eq!(events[0].start().as_micros(), 2_000_000);
    assert_eq!(events[0].end().as_micros(), 4_500_000);

    let later = DashTextFetchedSegment {
        documents: fetched.documents,
        player_shift_micros: 3_000_000,
    };
    let events = decode_documents(&later, LIMIT).expect("decodes");
    assert_eq!(events[0].start().as_micros(), 13_000_000);
}

#[test]
fn webvtt_documents_are_decoded_and_shifted_too() {
    let fetched = DashTextFetchedSegment {
        documents: vec![DashTextDocument {
            format: DashTextDocumentFormat::WebVtt,
            bytes: b"WEBVTT\n\n00:00:05.000 --> 00:00:07.000\nline\n\n".to_vec(),
            sample_interval: None,
        }],
        player_shift_micros: 1_000_000,
    };
    let events = decode_documents(&fetched, LIMIT).expect("decodes");
    assert_eq!(events[0].start().as_micros(), 6_000_000);
    assert_eq!(events[0].end().as_micros(), 8_000_000);
}

#[test]
fn a_broken_document_is_skipped_while_another_one_survives_and_all_broken_is_malformed() {
    let broken = DashTextDocument {
        format: DashTextDocumentFormat::Ttml,
        bytes: b"<tt xmlns=\"http://www.w3.org/ns/ttml\"><body".to_vec(),
        sample_interval: None,
    };
    let mixed = DashTextFetchedSegment {
        documents: vec![broken.clone(), ttml("00:00:01.000", "00:00:02.000", "ok")],
        player_shift_micros: 0,
    };
    let events = decode_documents(&mixed, LIMIT).expect("one good document is enough");
    assert_eq!(events.len(), 1);

    let all_broken = DashTextFetchedSegment {
        documents: vec![broken],
        player_shift_micros: 0,
    };
    assert_eq!(
        decode_documents(&all_broken, LIMIT),
        Err(StreamSubtitleError::Malformed)
    );
}

#[test]
fn an_empty_segment_has_no_events_and_is_not_an_error() {
    let fetched = DashTextFetchedSegment {
        documents: Vec::new(),
        player_shift_micros: 0,
    };
    assert_eq!(decode_documents(&fetched, LIMIT), Ok(Vec::new()));
}

#[test]
fn oversized_documents_report_the_limit_that_was_applied() {
    let fetched = DashTextFetchedSegment {
        documents: vec![ttml("00:00:01.000", "00:00:02.000", "ok")],
        player_shift_micros: 0,
    };
    let tiny = NonZeroUsize::new(16).unwrap();
    assert_eq!(
        decode_documents(&fetched, tiny),
        Err(StreamSubtitleError::TooLarge { limit_bytes: 16 })
    );
}

#[test]
fn shift_overflow_is_a_malformed_segment_not_a_panic() {
    let fetched = DashTextFetchedSegment {
        documents: vec![ttml("00:00:10.000", "00:00:12.000", "x")],
        player_shift_micros: i64::MAX,
    };
    assert_eq!(
        decode_documents(&fetched, LIMIT),
        Err(StreamSubtitleError::Malformed)
    );
}

#[test]
fn transport_errors_keep_their_distinct_meaning() {
    let cases = [
        (
            DashTextError::UnknownTrack,
            StreamSubtitleError::UnknownTrack,
        ),
        (DashTextError::Cancelled, StreamSubtitleError::Cancelled),
        (DashTextError::NotFound, StreamSubtitleError::NotFound),
        (DashTextError::Network, StreamSubtitleError::Network),
        (DashTextError::Rejected, StreamSubtitleError::Rejected),
        (
            DashTextError::TooLarge { limit_bytes: 7 },
            StreamSubtitleError::TooLarge { limit_bytes: 7 },
        ),
        (DashTextError::Malformed, StreamSubtitleError::Malformed),
        (DashTextError::Stale, StreamSubtitleError::Stale),
    ];
    for (source, expected) in cases {
        assert_eq!(StreamSubtitleError::from(source), expected);
    }
    assert!(StreamSubtitleError::from(DashTextError::Network).is_transient());
    assert!(StreamSubtitleError::from(DashTextError::Cancelled).is_cancellation());
    assert_eq!(
        StreamSubtitleError::from(DashTextError::NotFound).failure_kind(),
        Some(StreamSubtitleFailureKind::NotFound)
    );
}

#[test]
fn stpp_sample_document_is_clipped_to_its_sample_interval_before_the_shift() {
    // Реплика 0..10 с (media), сэмпл 2..4 с, сдвиг на плеер +100 с: видна 102..104 с.
    let mut document = ttml("00:00:00.000", "00:00:10.000", "clipped");
    document.sample_interval = Some(web_media_dash::DashTextSampleInterval {
        start_micros: 2_000_000,
        end_micros: 4_000_000,
    });
    let fetched = DashTextFetchedSegment {
        documents: vec![document],
        player_shift_micros: 100_000_000,
    };
    let events = decode_documents(&fetched, LIMIT).expect("decodes");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].start().as_micros(), 102_000_000);
    assert_eq!(events[0].end().as_micros(), 104_000_000);
}
