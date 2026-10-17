//! Заголовки субтитровых дорожек (`codec_private`) в `MediaStarted`: настоящий MKV и фильтр дорожек.

use super::*;

/// Настоящий MKV с ASS-дорожкой: заголовок (`[Script Info]`, стили) приходит в `MediaStarted`,
/// раньше пакетов, и принадлежит именно ASS-дорожке; видео/аудио заголовков не дают.
#[test]
fn real_mkv_with_ass_track_delivers_script_header_with_media_started() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles/ass-with-font.mkv");
    let demuxer =
        symphonia_demux::SymphoniaDemuxer::from_file(&fixture).expect("ass fixture must open");
    let ass_track_id = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .map(|track| track.id)
        .expect("fixture has an ASS subtitle track");
    let mut harness = harness_with_demuxer(1_024, Box::new(demuxer));

    run_ticks(&mut harness.session, 20);

    let messages = drain(&harness.receiver);
    let SubtitleFeedMessage::MediaStarted(start) = &messages[0] else {
        panic!(
            "the first message must be MediaStarted, got {:?}",
            messages[0]
        );
    };
    assert_eq!(
        start.track_headers.len(),
        1,
        "only the ASS track has a header"
    );
    assert_eq!(start.track_headers[0].track_id, ass_track_id);
    let header_text = String::from_utf8_lossy(&start.track_headers[0].codec_private);
    assert!(
        header_text.contains("[Script Info]"),
        "got header: {header_text}"
    );
    assert!(
        header_text.contains("Noto Sans Hebrew"),
        "style table is in the header"
    );
    assert!(
        packets_of(&messages)
            .iter()
            .any(|packet| packet.track_id == ass_track_id),
        "event packets follow the header"
    );
}

/// Дорожки без заголовка, пустой заголовок и не-субтитровые дорожки не попадают в `MediaStarted`.
#[test]
fn tracks_without_subtitle_header_produce_no_headers() {
    let mut subtitle_with_empty_header = fake_track(SUBTITLE_TRACK_B, TrackKind::Subtitle);
    subtitle_with_empty_header.codec_private = Some(Bytes::new());
    let mut video_with_private = fake_track(VIDEO_TRACK, TrackKind::Video);
    video_with_private.codec_private = Some(Bytes::from_static(b"avcC"));
    let mut subtitle_with_header = fake_track(SUBTITLE_TRACK_A, TrackKind::Subtitle);
    subtitle_with_header.codec_private = Some(Bytes::from_static(b"[Script Info]"));
    let tracks = vec![
        video_with_private,
        subtitle_with_header,
        subtitle_with_empty_header,
    ];

    let headers = crate::subtitle_feed::collect_subtitle_track_headers(&tracks);

    assert_eq!(headers.len(), 1);
    assert_eq!(
        headers[0].track_id,
        media_core::TrackId::new(SUBTITLE_TRACK_A)
    );
    assert_eq!(
        headers[0].codec_private,
        Bytes::from_static(b"[Script Info]")
    );
}
