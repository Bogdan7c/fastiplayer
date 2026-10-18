//! Модель: безопасный `Debug`, равенство источников, пустой источник.

use super::fake::{FakeTransport, index};
use crate::stream_subtitles::{
    StreamIndexRefresh, StreamSegment, StreamSegmentKey, StreamSegmentRef, StreamSubtitleSource,
    StreamSubtitleTrackId,
};

#[test]
fn source_exposes_track_metadata_and_compares_by_transport_identity() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        |_| Err(crate::stream_subtitles::StreamSubtitleError::Stale),
    );
    let source = StreamSubtitleSource::new(transport.clone()).expect("one track");
    assert_eq!(source.tracks().len(), 1);
    let track = source
        .track(StreamSubtitleTrackId::new(0))
        .expect("track 0");
    assert_eq!(track.language.as_deref(), Some("en"));
    assert_eq!(&*track.name, "English");
    assert!(source.track(StreamSubtitleTrackId::new(5)).is_none());
    assert_eq!(source.clone(), source);
    let other = StreamSubtitleSource::new(transport).expect("same transport");
    assert_eq!(other, source, "same transport -> same source");
}

#[test]
fn opaque_segment_reference_never_prints_its_payload() {
    let secret = "https://media.example.test/seg.vtt?token=TOPSECRET";
    let segment = StreamSegment {
        key: StreamSegmentKey::new(7),
        reference: StreamSegmentRef::new(secret.to_owned()),
        start: std::time::Duration::ZERO,
        duration: std::time::Duration::from_secs(6),
    };
    let debug = format!("{segment:?}");
    assert!(!debug.contains("TOPSECRET"));
    assert!(!debug.contains("media.example.test"));
    assert_eq!(
        segment
            .reference
            .downcast_ref::<String>()
            .map(String::as_str),
        Some(secret)
    );
}
