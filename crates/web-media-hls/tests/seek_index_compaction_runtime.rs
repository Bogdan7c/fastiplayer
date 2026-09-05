//! Production-path regression для bounded HLS VOD seek index compaction.

// Общий integration harness содержит fixtures для других HLS сценариев; этот target
// намеренно использует только TS subset и не должен дублировать сетевой/transport код.
#[allow(dead_code)]
mod support;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_muxed_ts_segment, open_policy,
    response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, SecretInlineMediaPlaylist, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

/// Hermetic worker deadline: timeout остаётся assertion-ом, а не скрытым retry budget.
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Строит точный muxed A/V selection без variant/audio fallback.
fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

/// Собирает production HLS VOD request с authoritative inline playlist.
fn inline_request(server: &TestServer, playlist: &str) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let target = server.target("/authoritative-inline.m3u8");
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::InlineMedia {
            selected_url: target,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        selection: muxed_selection(),
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

/// Ожидает следующий готовый event, проверяя nonblocking owner poll contract.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let call_started = Instant::now();
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(
                    call_started.elapsed() < Duration::from_millis(50),
                    "player-owner poll должен оставаться nonblocking"
                );
                assert!(
                    Instant::now() < deadline,
                    "HLS worker превысил test deadline"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return Ok(event),
        }
    }
}

/// Проецирует только стабильную public topology, не сравнивая private demux IDs.
fn track_signature(tracks: &[TrackInfo]) -> Vec<(TrackId, TrackKind)> {
    tracks.iter().map(|track| (track.id, track.kind)).collect()
}

/// Извлекает initial authoritative topology publication.
fn initial_track_signature(event: DemuxReadEvent) -> Vec<(TrackId, TrackKind)> {
    let DemuxReadEvent::TracksChanged(update) = event else {
        panic!("ожидался initial TracksChanged");
    };
    track_signature(&update.tracks)
}

/// Возвращает первый packet после seek и запрещает скрытую смену topology.
fn next_landing_packet(
    demuxer: &mut dyn Demuxer,
    stable_tracks: &[(TrackId, TrackKind)],
) -> Packet {
    loop {
        match next_ready_event(demuxer).expect("post-seek HLS event") {
            DemuxReadEvent::TracksChanged(update) => {
                assert_eq!(track_signature(&update.tracks), stable_tracks);
            }
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("seek достиг EOS до landing packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                unreachable!("next_ready_event уже фильтрует temporary readiness")
            }
        }
    }
}

/// Строит длинный finite playlist, где количество evidence превышает tiny budget.
fn six_segment_playlist() -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:30\n");
    for segment_index in 0..6 {
        playlist.push_str(&format!("#EXTINF:30,\nsegment-{segment_index}.ts\n"));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

/// После compaction поздний seek берёт точный хвост из HTTP либо completed RAM cache.
#[test]
fn late_seek_after_tiny_index_compaction_restarts_directly_from_latest_segment() {
    const SEGMENT_SECONDS: u64 = 30;
    const SEGMENT_COUNT: usize = 6;
    let segments = Arc::new(
        (0_u64..SEGMENT_COUNT as u64)
            .map(|segment_index| {
                long_muxed_ts_segment(
                    segment_index
                        .saturating_mul(SEGMENT_SECONDS)
                        .saturating_mul(90_000),
                    SEGMENT_SECONDS,
                )
            })
            .collect::<Vec<_>>(),
    );
    let server_segments = Arc::clone(&segments);
    let server = TestServer::start(move |_, request| {
        server_segments
            .iter()
            .enumerate()
            .find_map(|(segment_index, segment)| {
                request
                    .request_line
                    .contains(&format!("/segment-{segment_index}.ts"))
                    .then(|| response("200 OK", &[], segment))
            })
            .unwrap_or_else(|| response("404 Not Found", &[], b""))
    });
    let playlist = six_segment_playlist();
    let mut request = inline_request(&server, &playlist);
    request.policy.maximum_seek_index_entries =
        NonZeroUsize::new(4).expect("tiny muxed A/V seek budget");
    let opened = prepare_hls_vod(request).expect("prepare compacted-index HLS VOD");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(
        next_ready_event(&mut *demuxer).expect("initial compacted-index tracks"),
    );

    // Audio на 150 s может выйти раньше video RAP того же segment-а. Для
    // decode-safe seek требуется наблюдённый keyframe, а не опережающий audio PTS.
    loop {
        match next_ready_event(&mut *demuxer).expect("observe every HLS segment") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video
                    && packet.keyframe == PacketKeyframe::Keyframe
                    && packet.pts == Duration::from_secs(150) =>
            {
                break;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TracksChanged(update) => {
                assert_eq!(track_signature(&update.tracks), stable_tracks);
            }
            DemuxReadEvent::EndOfStream => panic!("VOD закончился до segment-5 evidence"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }

    let first_seek_request = server.requests().len();
    let seek = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            155,
        )))
        .expect("late decode-safe seek after compaction");
    assert_eq!(seek.actual_position.as_duration(), Duration::from_secs(150));
    let landing_packet = next_landing_packet(&mut *demuxer, &stable_tracks);
    assert_eq!(landing_packet.kind, TrackKind::Video);
    assert_eq!(landing_packet.keyframe, PacketKeyframe::Keyframe);
    assert_eq!(landing_packet.pts, Duration::from_secs(150));

    let seek_requests = &server.requests()[first_seek_request..];
    let media_requests = seek_requests
        .iter()
        .filter(|request| request.request_line.contains(".ts"))
        .map(|request| request.request_line.as_str())
        .collect::<Vec<_>>();
    assert!(
        media_requests.len() <= 1,
        "seek не должен повторно открывать exact tail: {media_requests:?}"
    );
    assert!(
        media_requests
            .iter()
            .all(|request_line| request_line.contains("/segment-5.ts")),
        "seek не должен читать промежуточные segments: {media_requests:?}"
    );
}
