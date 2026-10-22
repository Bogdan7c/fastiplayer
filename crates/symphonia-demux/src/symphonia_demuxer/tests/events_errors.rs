//! Порядок событий, EOF и обработка ошибок формата.

use super::*;

#[test]
fn metadata_revisions_precede_packets_without_changing_packet_order() {
    let first_revision = metadata_revision(vec![
        StandardTag::TrackTitle(Arc::new("Episode title".into())),
        StandardTag::DiscNumber(2),
        StandardTag::TrackNumber(8),
    ]);
    let second_revision = metadata_revision(vec![
        StandardTag::Album(Arc::new("Series collection".into())),
        StandardTag::TrackNumber(9),
        StandardTag::TvSeasonNumber(3),
        StandardTag::TvEpisodeNumber(11),
    ]);
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![
            Ok(small_vp9_keyframe_packet(1, 10)),
            Ok(small_vp9_keyframe_packet(1, 20)),
        ],
    )
    .with_metadata_revisions_after_packets(vec![first_revision, second_revision]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let first_metadata = demuxer
        .next_event()
        .expect("The first metadata revision should read");
    let first_packet = demuxer
        .next_event()
        .expect("The first packet must be saved after the metadata event.");
    let second_metadata = demuxer
        .next_event()
        .expect("The second metadata revision should read");
    let second_packet = demuxer
        .next_event()
        .expect("The second packet must maintain the original order.");

    let DemuxReadEvent::MediaMetadataChanged(first_metadata) = first_metadata else {
        panic!("The first metadata revision was expected.");
    };
    assert_eq!(first_metadata.tags.title.as_deref(), Some("Episode title"));
    assert_eq!(
        first_metadata.tags.disc_number,
        Some(media_core::DiscNumber::new(2))
    );
    assert_eq!(
        first_metadata.tags.track_number,
        Some(media_core::TrackNumber::new(8))
    );

    let DemuxReadEvent::Packet(first_packet) = first_packet else {
        panic!("The first packet after the first metadata revision");
    };
    assert_eq!(first_packet.pts, Duration::from_millis(10));

    let DemuxReadEvent::MediaMetadataChanged(second_metadata) = second_metadata else {
        panic!("The second metadata revision was expected.");
    };
    assert_eq!(second_metadata.tags.title.as_deref(), Some("Episode title"));
    assert_eq!(
        second_metadata.tags.album.as_deref(),
        Some("Series collection")
    );
    assert_eq!(
        second_metadata.tags.disc_number,
        Some(media_core::DiscNumber::new(2))
    );
    assert_eq!(
        second_metadata.tags.track_number,
        Some(media_core::TrackNumber::new(9))
    );
    assert_eq!(
        second_metadata.tags.tv_season_number,
        Some(media_core::TvSeasonNumber::new(3))
    );
    assert_eq!(
        second_metadata.tags.tv_episode_number,
        Some(media_core::TvEpisodeNumber::new(11))
    );

    let DemuxReadEvent::Packet(second_packet) = second_packet else {
        panic!("A second packet was expected after the second metadata revision.");
    };
    assert_eq!(second_packet.pts, Duration::from_millis(20));
}

/// Статичная metadata из заголовка не порождает событий на packet-ах, а live revision
/// поверх уже применённой всё равно замечается и сливается с ней как upsert.
#[test]
fn open_time_metadata_stays_silent_until_a_live_revision_arrives() {
    let open_revision = metadata_revision(vec![
        StandardTag::TrackTitle(Arc::new("Header title".into())),
        StandardTag::DiscNumber(1),
    ]);
    let live_revision =
        metadata_revision(vec![StandardTag::TrackTitle(Arc::new("Live title".into()))]);
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![
            Ok(small_vp9_keyframe_packet(1, 10)),
            Ok(small_vp9_keyframe_packet(1, 20)),
            Ok(small_vp9_keyframe_packet(1, 30)),
        ],
    )
    .with_open_metadata_revision(open_revision)
    .with_metadata_schedule_after_packets(vec![None, None, Some(live_revision)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let open_metadata = demuxer
        .media_metadata()
        .expect("Symphonia demuxer always publishes a metadata snapshot");
    assert_eq!(open_metadata.tags.title.as_deref(), Some("Header title"));

    let mut events = Vec::new();
    loop {
        let event = demuxer.next_event().expect("fake events must read");
        if event == DemuxReadEvent::EndOfStream {
            break;
        }
        events.push(event);
    }

    let [
        DemuxReadEvent::Packet(first_packet),
        DemuxReadEvent::Packet(second_packet),
        DemuxReadEvent::MediaMetadataChanged(live_metadata),
        DemuxReadEvent::Packet(third_packet),
    ] = events.as_slice()
    else {
        panic!("Expected two silent packets, then metadata before the third packet: {events:?}");
    };
    assert_eq!(first_packet.pts, Duration::from_millis(10));
    assert_eq!(second_packet.pts, Duration::from_millis(20));
    assert_eq!(third_packet.pts, Duration::from_millis(30));
    assert_eq!(live_metadata.tags.title.as_deref(), Some("Live title"));
    assert_eq!(
        live_metadata.tags.disc_number,
        Some(media_core::DiscNumber::new(1)),
        "live revision is an upsert over the header tags"
    );
    assert_eq!(demuxer.media_metadata().as_ref(), Some(live_metadata));
}

#[test]
fn normal_eof_returns_terminal_event_without_error() {
    let mut demuxer =
        fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());

    let event = demuxer
        .next_event()
        .expect("normal EOF shouldn't be a mistake");

    assert_eq!(event, DemuxReadEvent::EndOfStream);
}

#[test]
fn seek_preserves_pending_tracks_changed_event_with_fake_reader() {
    // Fake reader делает lifecycle contract hermetic: для проверки не нужен media asset или filesystem.
    let mut demuxer =
        fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());
    // Событие имитирует lifecycle update, который уже был поставлен в очередь до следующего seek.
    let retained_update = DemuxTrackListUpdate::new(demuxer.tracks().to_vec(), demuxer.duration());
    demuxer
        .pending_events
        .push_back(DemuxReadEvent::TracksChanged(retained_update.clone()));
    // Seek обязан временно снять lifecycle events, затем вернуть их перед результатами format reader-а.
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect("fake seek must preserve pending lifecycle event");
    // Первый observed event доказывает порядок и отсутствие тихой потери queued update.
    match demuxer
        .next_event()
        .expect("retained TracksChanged must be readable after fake seek")
    {
        DemuxReadEvent::TracksChanged(actual_update) => assert_eq!(actual_update, retained_update),
        unexpected_event => panic!("expected retained TracksChanged, got {unexpected_event:?}"),
    }
}

#[test]
fn unknown_track_does_not_break_open() {
    let reader = FakeFormatReader::new(vec![unknown_track(9)], Vec::new());
    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("unknown track must not break open");

    assert!(demuxer.tracks().is_empty());
}

#[test]
fn subtitle_packets_are_delivered_as_subtitle_kind_packets() {
    let reader = FakeFormatReader::new(
        vec![subtitle_track(9)],
        vec![Ok(fake_packet(9, 0, b"subtitle".to_vec()))],
    );
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The subtitle track should not break open.");

    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Subtitle);

    let event = demuxer
        .next_event()
        .expect("subtitle packet must be delivered without fatal errors");

    let DemuxReadEvent::Packet(packet) = event else {
        panic!("expected subtitle packet, got {event:?}");
    };
    assert_eq!(packet.kind, TrackKind::Subtitle);
    assert_eq!(packet.track_id, TrackId::new(9));
    assert_eq!(&packet.data[..], b"subtitle");
    assert_eq!(
        demuxer.next_event().expect("eof"),
        DemuxReadEvent::EndOfStream
    );
}

#[test]
fn packets_of_unclassified_tracks_are_skipped_without_unknown_track_error() {
    let reader = FakeFormatReader::new(
        vec![unknown_track(9)],
        vec![Ok(fake_packet(9, 0, b"opaque".to_vec()))],
    );
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The unclassified track should not break open.");

    let event = demuxer
        .next_event()
        .expect("unclassified packet must be missed without fatal errors");

    assert_eq!(event, DemuxReadEvent::EndOfStream);
}

#[test]
fn unexpected_eof_error_is_kept_as_defensive_eof_fallback() {
    let mut demuxer = fake_demuxer_with_options(
        vec![Err(SymphoniaError::IoError(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "legacy eof",
        )))],
        HashMap::new(),
        DemuxerOptions::default(),
    );

    let event = demuxer
        .next_event()
        .expect("defensive UnexpectedEof fallback must remain EOF");

    assert_eq!(event, DemuxReadEvent::EndOfStream);
}

#[test]
fn decode_error_from_format_reader_is_parse_error_without_retry() {
    let options =
        DemuxerOptions::from_max_consecutive_corrupted_packets(2).expect("test limit non-zero");
    let next_packet_call_count = Arc::new(Mutex::new(0));
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![
            Err(SymphoniaError::DecodeError("isomp4: no atom pending read")),
            Ok(small_vp9_keyframe_packet(1, 10)),
        ],
    )
    .with_next_packet_call_count(next_packet_call_count.clone());
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        options,
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .next_event()
        .expect_err("structural DecodeError from next_event must have fatal");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("fatal must remain typed DemuxError");

    match demux_error {
        DemuxError::Parse(SymphoniaError::DecodeError(reason)) => {
            assert_eq!(*reason, "isomp4: no atom pending read");
        }
        unexpected_error => panic!("expected Parse(DecodeError), received {unexpected_error:?}"),
    }

    assert_eq!(
        *next_packet_call_count
            .lock()
            .expect("next_packet call count mutex should not be poisoned"),
        1
    );
}

#[test]
fn packet_for_unknown_track_is_fatal() {
    let mut demuxer = fake_demuxer_with_options(
        vec![Ok(fake_packet(99, 0, b"\x00".to_vec()))],
        HashMap::new(),
        DemuxerOptions::default(),
    );

    let error = demuxer
        .next_event()
        .expect_err("unknown track must have fatal");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("fatal must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::UnknownPacketTrack { track_id: 99 }
    ));
}

#[test]
fn uncertain_vp9_keyframe_probe_is_returned_without_demux_error() {
    let matroska_tracks = HashMap::from([(
        TrackId::new(1),
        MatroskaVideoTrack {
            codec_id: Some("V_VP9".to_string()),
            metadata: None,
        },
    )]);
    let mut demuxer = fake_demuxer_with_options(
        vec![
            Ok(fake_packet(1, 0, b"\x00".to_vec())),
            Ok(fake_packet(1, 10, b"\x00".to_vec())),
            Ok(small_vp9_keyframe_packet(1, 20)),
        ],
        matroska_tracks,
        DemuxerOptions::default(),
    );

    let first_event = demuxer
        .next_event()
        .expect("An uncertain keyframe sample should not become fatal corruption.");
    let DemuxReadEvent::Packet(first_packet) = first_event else {
        panic!("packet unknown keyframe must be returned: {first_event:?}");
    };
    let second_event = demuxer
        .next_event()
        .expect("Repeated unsure keyframe should not accumulate corruption counter");
    let DemuxReadEvent::Packet(second_packet) = second_event else {
        panic!("Second packet with unknown keyframe must be returned: {second_event:?}");
    };

    assert_eq!(first_packet.pts, Duration::ZERO);
    assert_eq!(first_packet.keyframe, PacketKeyframe::Unknown);
    assert_eq!(second_packet.pts, Duration::from_millis(10));
    assert_eq!(second_packet.keyframe, PacketKeyframe::Unknown);
}
