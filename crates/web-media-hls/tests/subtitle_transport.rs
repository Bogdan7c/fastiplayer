//! Hermetic evidence для `HlsSubtitleTransport`: playlist, сегменты, типизированные ошибки.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
};
use source_core::CancellationToken;
use support::{
    ObservedRequest, TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts,
    open_policy, range_response, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsSubtitleFetchError, HlsSubtitleSegmentByteLimit,
    HlsSubtitleTransport, HlsSubtitleUnsupportedFeature, HlsTimestampOrigins,
    HlsVariantSelectionIntent, HlsVodOpenRequest, prepare_hls_live, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);
/// Секреты в query: ни один из них не имеет права попасть в Debug/Display.
const PLAYLIST_SECRET: &str = "PLAYLISTSECRET";
const SEGMENT_SECRET: &str = "SEGMENTSECRET";

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn master_with_subtitles() -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",\
         DEFAULT=YES,AUTOSELECT=YES,FORCED=YES,CHARACTERISTICS=\"public.accessibility.transcribes-spoken-dialog\",\
         URI=\"subs/en.m3u8?token={PLAYLIST_SECRET}\"\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Deutsch\",LANGUAGE=\"de\",\
         URI=\"subs/de.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,SUBTITLES=\"subs\"\nvideo.m3u8\n"
    )
}

fn vod_request(server: &TestServer) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::Fetch {
            selected_url: target,
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

/// Обычный ответ видео-части мастера; всё остальное делегируется `subtitle_handler`.
fn serve(
    request: &ObservedRequest,
    video: &[u8],
    subtitle_handler: &dyn Fn(&ObservedRequest) -> Option<Vec<u8>>,
) -> Vec<u8> {
    let line = &request.request_line;
    if line.contains("/master.m3u8") {
        response("200 OK", &[], master_with_subtitles().as_bytes())
    } else if line.contains("/video.m3u8") {
        response(
            "200 OK",
            &[],
            b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nvideo.ts\n#EXT-X-ENDLIST\n",
        )
    } else if line.contains("/video.ts") {
        response("200 OK", &[], video)
    } else {
        subtitle_handler(request).unwrap_or_else(|| response("404 Not Found", &[], b""))
    }
}

fn subtitle_requests(server: &TestServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .map(|request| request.request_line)
        .filter(|line| line.contains("/subs/"))
        .collect()
}

fn byte_limit(bytes: usize) -> HlsSubtitleSegmentByteLimit {
    HlsSubtitleSegmentByteLimit::from_bytes(NonZeroUsize::new(bytes).expect("non-zero limit"))
}

/// Дочитывает открытие, чтобы доказать: сам open субтитры не трогает.
fn wait_for_tracks(demuxer: &mut dyn Demuxer) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        match demuxer.next_event().expect("open stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::TracksChanged(_) => return,
            _ => {}
        }
    }
    panic!("HLS open did not publish tracks in time");
}

/// Возвращает транспорт вместе с живым демуксером: отмена источника привязана к его жизни,
/// как и в плеере (закрыли media - субтитры этого media больше ничего не качают).
fn open_transport(server: &TestServer) -> (HlsSubtitleTransport, Box<dyn Demuxer + Send>) {
    let opened = prepare_hls_vod(vod_request(server)).expect("prepare VOD with subtitles");
    let transport = opened.subtitle_transport();
    let mut demuxer = opened.into_demuxer();
    wait_for_tracks(&mut *demuxer);
    (transport, demuxer)
}

#[test]
fn transport_and_origins_handles_are_thread_safe_values() {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    assert_send_sync_clone::<HlsSubtitleTransport>();
    assert_send_sync_clone::<HlsTimestampOrigins>();
}

#[test]
fn vod_open_alone_never_requests_subtitles_then_loads_playlist_and_segment_bytes() {
    let video = Arc::new(muxed_ts(90_000));
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            let line = &request.request_line;
            if line.contains("/subs/en.m3u8") {
                Some(response(
                    "200 OK",
                    &[],
                    format!(
                        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:7\n\
                         #EXTINF:2.5,\ns0.vtt?sig={SEGMENT_SECRET}\n\
                         #EXT-X-DISCONTINUITY\n#EXTINF:3.0,\ns1.vtt\n#EXT-X-ENDLIST\n"
                    )
                    .as_bytes(),
                ))
            } else if line.contains("/subs/s0.vtt?sig=SEGMENTSECRET") {
                Some(response("200 OK", &[], b"WEBVTT\n\nfirst"))
            } else if line.contains("/subs/s1.vtt") {
                Some(response("200 OK", &[], b"WEBVTT\n\nsecond"))
            } else {
                None
            }
        })
    });
    let (transport, _demuxer) = open_transport(&server);

    // Открытие видео не обязано и не имеет права трогать subtitle-ресурсы.
    assert!(subtitle_requests(&server).is_empty());

    let renditions = transport.renditions();
    assert_eq!(renditions.len(), 2);
    let english = &renditions[0];
    assert_eq!(&*english.name, "English");
    assert_eq!(english.language.as_deref(), Some("en"));
    assert!(english.forced && english.default && english.autoselect);
    assert_eq!(&*english.group_id, "subs");
    assert_eq!(
        english.characteristics.as_deref(),
        Some("public.accessibility.transcribes-spoken-dialog")
    );
    let german = &renditions[1];
    assert!(!german.forced && !german.default && !german.autoselect);
    assert_ne!(english.id, german.id);

    let cancellation = CancellationToken::new();
    let snapshot = transport
        .load_playlist(english.id, &cancellation)
        .expect("VOD subtitle playlist");
    assert!(!snapshot.is_live);
    assert_eq!(snapshot.target_duration, Duration::from_secs(3));
    assert_eq!(snapshot.segments.len(), 2);
    assert_eq!(snapshot.segments[0].media_sequence, 7);
    assert_eq!(snapshot.segments[0].discontinuity_sequence, 0);
    assert_eq!(snapshot.segments[0].start, Duration::ZERO);
    assert_eq!(snapshot.segments[0].duration, Duration::from_millis(2_500));
    assert_eq!(snapshot.segments[1].media_sequence, 8);
    assert_eq!(snapshot.segments[1].discontinuity_sequence, 1);
    assert_eq!(snapshot.segments[1].start, Duration::from_millis(2_500));
    assert_eq!(snapshot.segments[1].duration, Duration::from_secs(3));
    assert_eq!(snapshot.total_duration(), Duration::from_millis(5_500));
    assert_eq!(subtitle_requests(&server).len(), 1);

    // Query сегмента и playlist-а дошла до сервера (иначе подписанные CDN не заработают).
    let first = transport
        .fetch_segment(
            &snapshot.segments[0].reference,
            byte_limit(1_024),
            &cancellation,
        )
        .expect("first segment bytes");
    assert_eq!(first, b"WEBVTT\n\nfirst");
    let second = transport
        .fetch_segment(
            &snapshot.segments[1].reference,
            byte_limit(1_024),
            &cancellation,
        )
        .expect("second segment bytes");
    assert_eq!(second, b"WEBVTT\n\nsecond");
    assert!(
        subtitle_requests(&server)
            .iter()
            .any(|line| line.contains(&format!("token={PLAYLIST_SECRET}")))
    );

    // Секреты не утекают ни в один Debug этой границы.
    let debug = format!(
        "{transport:?} {snapshot:?} {:?} {renditions:?}",
        snapshot.segments[0].reference
    );
    for forbidden in [
        PLAYLIST_SECRET,
        SEGMENT_SECRET,
        "127.0.0.1",
        "token=",
        "sig=",
    ] {
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
    }
}

#[test]
fn byte_range_segments_fetch_exact_slices_including_implicit_offset() {
    let video = Arc::new(muxed_ts(90_000));
    let blob = b"WEBVTT-AAAA-BBBBB".to_vec();
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            let line = &request.request_line;
            if line.contains("/subs/en.m3u8") {
                Some(response(
                    "200 OK",
                    &[],
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
                      #EXTINF:2,\n#EXT-X-BYTERANGE:11@0\nall.vtt\n\
                      #EXTINF:2,\n#EXT-X-BYTERANGE:6\nall.vtt\n#EXT-X-ENDLIST\n",
                ))
            } else if line.contains("/subs/all.vtt") {
                Some(range_response(request, &blob))
            } else {
                None
            }
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let id = transport.renditions()[0].id;
    let cancellation = CancellationToken::new();
    let snapshot = transport
        .load_playlist(id, &cancellation)
        .expect("playlist");
    let slices = snapshot
        .segments
        .iter()
        .map(|segment| {
            transport
                .fetch_segment(&segment.reference, byte_limit(64), &cancellation)
                .expect("range segment")
        })
        .collect::<Vec<_>>();
    assert_eq!(slices[0], b"WEBVTT-AAAA");
    assert_eq!(slices[1], b"-BBBBB");
}

#[test]
fn live_playlist_reloads_see_new_segments_until_endlist() {
    let video = Arc::new(muxed_ts(90_000));
    let loads = Arc::new(AtomicUsize::new(0));
    let handler_loads = Arc::clone(&loads);
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            if !request.request_line.contains("/subs/en.m3u8") {
                return None;
            }
            let load = handler_loads.fetch_add(1, Ordering::SeqCst);
            let body = match load {
                0 => {
                    "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n\
                      #EXTINF:2,\na.vtt\n#EXTINF:2,\nb.vtt\n"
                }
                1 => {
                    "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:1\n\
                      #EXTINF:2,\nb.vtt\n#EXTINF:2,\nc.vtt\n#EXTINF:2,\nd.vtt\n"
                }
                _ => {
                    "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:3\n\
                      #EXTINF:2,\nd.vtt\n#EXT-X-ENDLIST\n"
                }
            };
            Some(response("200 OK", &[], body.as_bytes()))
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let id = transport.renditions()[0].id;
    let cancellation = CancellationToken::new();

    let first = transport
        .load_playlist(id, &cancellation)
        .expect("first load");
    assert!(first.is_live, "no ENDLIST means a growing playlist");
    let first_sequences = first
        .segments
        .iter()
        .map(|s| s.media_sequence)
        .collect::<Vec<_>>();
    assert_eq!(first_sequences, [0, 1]);

    let second = transport
        .load_playlist(id, &cancellation)
        .expect("second load");
    assert!(second.is_live);
    let second_sequences = second
        .segments
        .iter()
        .map(|s| s.media_sequence)
        .collect::<Vec<_>>();
    assert_eq!(
        second_sequences,
        [1, 2, 3],
        "second load sees the new segments"
    );
    // Скользящее окно: сегмент `b` (media sequence 1) остался на своём месте шкалы (2 с),
    // а не «уехал к нулю» вместе с уходом сегмента `a`.
    let starts = |snapshot: &web_media_hls::HlsSubtitlePlaylistSnapshot| {
        snapshot
            .segments
            .iter()
            .map(|s| (s.media_sequence, s.start.as_secs()))
            .collect::<Vec<_>>()
    };
    assert_eq!(starts(&first), [(0, 0), (1, 2)]);
    assert_eq!(starts(&second), [(1, 2), (2, 4), (3, 6)]);
    assert_eq!(second.timeline_epoch, first.timeline_epoch);

    let third = transport
        .load_playlist(id, &cancellation)
        .expect("third load");
    assert!(!third.is_live, "ENDLIST closes the playlist");
    assert_eq!(starts(&third), [(3, 6)]);
    assert_eq!(loads.load(Ordering::SeqCst), 3);
}

#[test]
fn live_reload_that_skips_the_whole_window_continues_the_timeline_and_changes_the_epoch() {
    let video = Arc::new(muxed_ts(90_000));
    let loads = Arc::new(AtomicUsize::new(0));
    let handler_loads = Arc::clone(&loads);
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            if !request.request_line.contains("/subs/en.m3u8") {
                return None;
            }
            let body = if handler_loads.fetch_add(1, Ordering::SeqCst) == 0 {
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n\
                  #EXTINF:2,\na.vtt\n#EXTINF:2,\nb.vtt\n"
            } else {
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:50\n\
                  #EXTINF:2,\nz.vtt\n"
            };
            Some(response("200 OK", &[], body.as_bytes()))
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let id = transport.renditions()[0].id;
    let cancellation = CancellationToken::new();
    let first = transport.load_playlist(id, &cancellation).expect("first");
    let gap = transport.load_playlist(id, &cancellation).expect("gap");
    // Шкала продолжается от конца прошлого снимка (4 с), а не прыгает в ноль.
    assert_eq!(gap.segments[0].start, Duration::from_secs(4));
    assert!(
        gap.timeline_epoch > first.timeline_epoch,
        "consumers must learn that the old playlist->player offset is stale"
    );
}

#[test]
fn http_failures_are_typed_and_distinct() {
    let video = Arc::new(muxed_ts(90_000));
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            let line = &request.request_line;
            if line.contains("/subs/en.m3u8") {
                Some(response(
                    "200 OK",
                    &[],
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
                      #EXTINF:2,\nmissing.vtt\n#EXTINF:2,\ngone.vtt\n\
                      #EXTINF:2,\nbroken.vtt\n#EXTINF:2,\nbig.vtt\n#EXT-X-ENDLIST\n",
                ))
            } else if line.contains("/subs/de.m3u8") || line.contains("/subs/gone.vtt") {
                Some(response("410 Gone", &[], b""))
            } else if line.contains("/subs/broken.vtt") {
                Some(response("403 Forbidden", &[], b""))
            } else if line.contains("/subs/big.vtt") {
                Some(response("200 OK", &[], b"0123456789"))
            } else {
                None
            }
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let renditions = transport.renditions();
    let cancellation = CancellationToken::new();
    let snapshot = transport
        .load_playlist(renditions[0].id, &cancellation)
        .expect("playlist");
    let fetch = |index: usize, limit: usize| {
        transport.fetch_segment(
            &snapshot.segments[index].reference,
            byte_limit(limit),
            &cancellation,
        )
    };
    assert_eq!(fetch(0, 64), Err(HlsSubtitleFetchError::NotFound));
    assert_eq!(fetch(1, 64), Err(HlsSubtitleFetchError::Gone));
    assert_eq!(
        fetch(2, 64),
        Err(HlsSubtitleFetchError::HttpStatus { status: 403 })
    );
    assert_eq!(
        fetch(3, 4),
        Err(HlsSubtitleFetchError::TooLarge { limit_bytes: 4 })
    );
    assert_eq!(fetch(3, 10).expect("exact fit"), b"0123456789");
    // Playlist rendition-а, который сервер снял с публикации.
    assert_eq!(
        transport.load_playlist(renditions[1].id, &cancellation),
        Err(HlsSubtitleFetchError::Gone)
    );
    // Тексты ошибок не содержат адресов.
    for error in [
        HlsSubtitleFetchError::NotFound,
        HlsSubtitleFetchError::TooLarge { limit_bytes: 4 },
    ] {
        assert!(!error.to_string().contains("127.0.0.1"));
    }
}

#[test]
fn malformed_master_instead_of_media_and_encrypted_playlists_are_typed() {
    let video = Arc::new(muxed_ts(90_000));
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            let line = &request.request_line;
            if line.contains("/subs/en.m3u8") {
                Some(response(
                    "200 OK",
                    &[],
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
                      #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
                      #EXTINF:2,\na.vtt\n#EXT-X-ENDLIST\n",
                ))
            } else if line.contains("/subs/de.m3u8") {
                Some(response("200 OK", &[], b"this is not a playlist"))
            } else {
                None
            }
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let renditions = transport.renditions();
    let cancellation = CancellationToken::new();
    assert_eq!(
        transport.load_playlist(renditions[0].id, &cancellation),
        Err(HlsSubtitleFetchError::Unsupported(
            HlsSubtitleUnsupportedFeature::Encryption
        ))
    );
    assert_eq!(
        transport.load_playlist(renditions[1].id, &cancellation),
        Err(HlsSubtitleFetchError::MalformedPlaylist)
    );
}

#[test]
fn method_none_key_is_not_treated_as_encryption() {
    let video = Arc::new(muxed_ts(90_000));
    let server = TestServer::start(move |_, request| {
        serve(request, &video, &|request| {
            request.request_line.contains("/subs/en.m3u8").then(|| {
                response(
                    "200 OK",
                    &[],
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=NONE\n\
                      #EXTINF:2,\na.vtt\n#EXT-X-ENDLIST\n",
                )
            })
        })
    });
    let (transport, _demuxer) = open_transport(&server);
    let snapshot = transport
        .load_playlist(transport.renditions()[0].id, &CancellationToken::new())
        .expect("plain playlist with METHOD=NONE");
    assert_eq!(snapshot.segments.len(), 1);
}

#[test]
fn cancelled_token_stops_before_any_request_and_foreign_handles_are_rejected() {
    let video = Arc::new(muxed_ts(90_000));
    let make_server = || {
        let video = Arc::clone(&video);
        TestServer::start(move |_, request| {
            serve(request, &video, &|request| {
                if request.request_line.contains("/subs/en.m3u8") {
                    Some(response(
                        "200 OK",
                        &[],
                        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.vtt\n#EXT-X-ENDLIST\n",
                    ))
                } else {
                    None
                }
            })
        })
    };
    let server = make_server();
    let other_server = make_server();
    let (transport, _demuxer) = open_transport(&server);
    let (other_transport, _other_demuxer) = open_transport(&other_server);
    let id = transport.renditions()[0].id;

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let before = subtitle_requests(&server).len();
    assert_eq!(
        transport.load_playlist(id, &cancelled),
        Err(HlsSubtitleFetchError::Cancelled)
    );
    assert_eq!(
        subtitle_requests(&server).len(),
        before,
        "no request after cancel"
    );

    let live = CancellationToken::new();
    let snapshot = transport.load_playlist(id, &live).expect("playlist");
    let reference = &snapshot.segments[0].reference;
    // Ссылка чужого транспорта и чужой индекс не принимаются и не создают запросов.
    let other_requests = subtitle_requests(&other_server).len();
    assert_eq!(
        other_transport.fetch_segment(reference, byte_limit(64), &live),
        Err(HlsSubtitleFetchError::Stale)
    );
    assert_eq!(subtitle_requests(&other_server).len(), other_requests);
    assert_eq!(
        transport.fetch_segment(reference, byte_limit(64), &cancelled),
        Err(HlsSubtitleFetchError::Cancelled)
    );
}

#[test]
fn unknown_rendition_id_and_dead_server_are_typed() {
    let video = Arc::new(muxed_ts(90_000));
    let server = TestServer::start(move |_, request| serve(request, &video, &|_| None));
    let (transport, _demuxer) = open_transport(&server);
    let cancellation = CancellationToken::new();
    let id = transport.renditions()[1].id;

    // Транспорт без второй дорожки: индекс из списка другого транспорта.
    let single_video = Arc::new(muxed_ts(90_000));
    let single_server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo.m3u8\n",
            )
        } else {
            serve(request, &single_video, &|_| None)
        }
    });
    let (empty_transport, _empty_demuxer) = open_transport(&single_server);
    assert!(empty_transport.renditions().is_empty());
    assert_eq!(
        empty_transport.load_playlist(id, &cancellation),
        Err(HlsSubtitleFetchError::UnknownRendition)
    );

    // Сервер умер после открытия: сеть, а не паника и не «не найдено».
    drop(server);
    assert_eq!(
        transport.load_playlist(id, &cancellation),
        Err(HlsSubtitleFetchError::Network)
    );
}

struct NoRefreshPort;

impl HlsEndpointRefreshPort for NoRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::AttemptsExhausted)
    }
}

#[test]
fn live_open_exposes_working_subtitle_transport_without_requesting_it() {
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let server = TestServer::start(move |_, request| {
        let line = &request.request_line;
        if line.contains("/master.m3u8") {
            response("200 OK", &[], master_with_subtitles().as_bytes())
        } else if line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
                  #EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n",
            )
        } else if line.contains("/a.ts") {
            response("200 OK", &[], &first)
        } else if line.contains("/b.ts") {
            response("200 OK", &[], &second)
        } else if line.contains("/subs/en.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:40\n#EXTINF:2,\nlive.vtt\n",
            )
        } else if line.contains("/subs/live.vtt") {
            response("200 OK", &[], b"WEBVTT\n\nlive")
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    let cancellation = CancellationToken::new();
    let request = HlsLiveOpenRequest {
        common: HlsVodOpenRequest {
            http: adaptive_context(
                &target,
                cancellation.clone(),
                generation,
                TestQueries::default(),
            ),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: target,
            },
            selection: muxed_selection(),
            overrides: HlsRequestOverrides::new(None),
            containers: HlsComponentContainerIntent {
                main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
                alternate_audio: None,
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh: Arc::new(NoRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    };
    let opened = prepare_hls_live(request).expect("prepare live with subtitles");
    let transport = opened.subtitle_transport();
    assert_eq!(opened.subtitle_renditions().len(), 2);
    assert!(
        subtitle_requests(&server).is_empty(),
        "open must not fetch subtitles"
    );

    let snapshot = transport
        .load_playlist(transport.renditions()[0].id, &CancellationToken::new())
        .expect("live subtitle playlist");
    assert!(snapshot.is_live);
    assert_eq!(snapshot.segments[0].media_sequence, 40);
    let bytes = transport
        .fetch_segment(
            &snapshot.segments[0].reference,
            byte_limit(1_024),
            &CancellationToken::new(),
        )
        .expect("live subtitle segment");
    assert_eq!(bytes, b"WEBVTT\n\nlive");
    // Отмена источника останавливает и транспорт субтитров.
    cancellation.cancel();
    assert_eq!(
        transport.load_playlist(transport.renditions()[0].id, &CancellationToken::new()),
        Err(HlsSubtitleFetchError::Cancelled)
    );
}
