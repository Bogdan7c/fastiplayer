//! Runtime-регрессия: поздний receipted seek стартует у manifest target, а не сканирует VOD с начала.

#[allow(dead_code)]
mod support;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use demux_api::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, PacketKeyframe, TrackId, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_audio_ts_segment,
    long_interleaved_muxed_ts_segment, long_muxed_ts_segment, long_muxed_ts_segment_without_rap,
    long_video_ts_segment, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsComponentContainerIntent,
    HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides,
    HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest, SecretInlineMediaPlaylist,
    prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const SEGMENT_SECONDS: u64 = 10;
const SEGMENT_COUNT: u64 = 8;

/// Собирает public request с inline playlist и изолированным test transport context.
fn inline_request(server: &TestServer, playlist: &str) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let selected_url = server.target("/manifest-owned-seek.m3u8");
    let mut policy = open_policy();
    // Маленькая очередь останавливает initial worker внутри первого сегмента до seek-команды.
    policy.progressive_limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(4).expect("progressive event capacity"),
        NonZeroUsize::new(64 * 1_024).expect("progressive packet bytes"),
    );
    HlsVodOpenRequest {
        http: adaptive_context(
            &selected_url,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::InlineMedia {
            selected_url,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        selection: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy,
    }
}

/// Собирает request для раздельных video/audio media playlists одного master-а.
fn separate_av_request(server: &TestServer) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let selected_url = server.target("/master.m3u8");
    let mut policy = open_policy();
    policy.progressive_limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(4).expect("progressive event capacity"),
        NonZeroUsize::new(64 * 1_024).expect("progressive packet bytes"),
    );
    HlsVodOpenRequest {
        http: adaptive_context(
            &selected_url,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::Fetch { selected_url },
        selection: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
                name: Some("Test audio".into()),
                ..HlsAudioRenditionEvidence::default()
            }),
            main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: Some(HlsContainerEvidence::Exact(
                HlsRequiredContainer::TransportStream,
            )),
        },
        demux_registry: demux_registry(),
        policy,
    }
}

/// Ждёт только readiness-события, не превращая `TemporarilyUnavailable` в blocking player call.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS worker readiness timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return Ok(event),
        }
    }
}

/// Возвращает stable public topology из обязательного initial `TracksChanged`.
fn initial_track_signature(demuxer: &mut dyn Demuxer) -> Vec<(TrackId, TrackKind)> {
    let DemuxReadEvent::TracksChanged(update) =
        next_ready_event(demuxer).expect("initial HLS tracks must be readable")
    else {
        panic!("initial HLS event must publish tracks");
    };
    update
        .tracks
        .into_iter()
        .map(|track| (track.id, track.kind))
        .collect()
}

/// Поллит authoritative worker receipt в рамках deterministic test deadline.
fn wait_for_receipt(
    handle: &demux_api::ProgressiveAsyncSeekHandle,
) -> demux_api::ProgressiveAsyncSeekReceipt {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(receipt) = handle.poll_receipt() {
            return receipt;
        }
        assert!(Instant::now() < deadline, "receipted HLS seek timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Генерирует простой media playlist, где каждый manifest segment владеет 10 секундами timeline.
fn playlist_text() -> String {
    let segment_lines = (0..SEGMENT_COUNT)
        .map(|segment_index| format!("#EXTINF:{SEGMENT_SECONDS},\nsegment-{segment_index}.ts\n"))
        .collect::<String>();
    format!("#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}\n{segment_lines}#EXT-X-ENDLIST\n")
}

/// Генерирует media playlist с заданным префиксом segment URI.
fn component_playlist_text(prefix: &str) -> String {
    let segment_lines = (0..SEGMENT_COUNT)
        .map(|segment_index| format!("#EXTINF:{SEGMENT_SECONDS},\n{prefix}-{segment_index}.ts\n"))
        .collect::<String>();
    format!("#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}\n{segment_lines}#EXT-X-ENDLIST\n")
}

#[test]
fn late_receipted_seek_fetches_target_segment_and_publishes_landing_packet() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let segment_start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                if segment_index == 6 {
                    long_interleaved_muxed_ts_segment(segment_start_pts)
                } else {
                    long_muxed_ts_segment(segment_start_pts, SEGMENT_SECONDS)
                }
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
    let playlist = playlist_text();
    let opened = prepare_hls_vod_receipted(
        inline_request(&server, &playlist),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare receipted HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("receipted HLS seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    assert_eq!(
        stable_tracks
            .iter()
            .filter(|(_, kind)| *kind == TrackKind::Video)
            .count(),
        1
    );
    assert_eq!(
        stable_tracks
            .iter()
            .filter(|(_, kind)| *kind == TrackKind::Audio)
            .count(),
        1
    );

    let requests_before_seek = server.requests().len();
    let fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            fence,
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue late worker seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("late manifest seek must succeed: {receipt:?}");
    };
    assert_eq!(
        result.actual_position.as_duration(),
        Duration::from_secs(60)
    );

    let seek_request_lines = server
        .requests()
        .into_iter()
        .skip(requests_before_seek)
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/segment-6.ts")),
        "seek должен открыть target segment: {seek_request_lines:?}"
    );
    for skipped_segment in 1..6 {
        assert!(
            seek_request_lines
                .iter()
                .all(|line| !line.contains(&format!("/segment-{skipped_segment}.ts"))),
            "seek не должен последовательно читать segment-{skipped_segment}: {seek_request_lines:?}"
        );
    }

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match next_ready_event(&mut *demuxer).expect("post-seek HLS event") {
            DemuxReadEvent::TracksChanged(update) => {
                let replacement_tracks = update
                    .tracks
                    .into_iter()
                    .map(|track| (track.id, track.kind))
                    .collect::<Vec<_>>();
                assert_eq!(replacement_tracks, stable_tracks);
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
                assert_eq!(packet.pts, Duration::from_secs(60));
                break;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("HLS ended before post-seek video packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(
            Instant::now() < deadline,
            "post-seek landing packet timed out"
        );
    }
}

#[test]
fn receipted_seek_without_near_target_rap_falls_back_to_proven_exact_anchor() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                if segment_index >= 5 {
                    long_muxed_ts_segment_without_rap(start_pts, SEGMENT_SECONDS)
                } else {
                    long_muxed_ts_segment(start_pts, SEGMENT_SECONDS)
                }
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
    let opened = prepare_hls_vod_receipted(
        inline_request(&server, &playlist_text()),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare fallback HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("receipted fallback seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    let requests_before_seek = server.requests().len();

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue fallback worker seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("fallback seek must preserve successful legacy outcome: {receipt:?}");
    };
    assert_eq!(result.actual_position.as_duration(), Duration::ZERO);

    let seek_request_lines = server
        .requests()
        .into_iter()
        .skip(requests_before_seek)
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/segment-6.ts")),
        "first candidate должен проверить target segment: {seek_request_lines:?}"
    );
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/segment-5.ts")),
        "second candidate должен проверить previous segment: {seek_request_lines:?}"
    );
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/segment-0.ts")),
        "после недоказанных candidates нужен legacy exact-anchor restart: {seek_request_lines:?}"
    );

    loop {
        match next_ready_event(&mut *demuxer).expect("fallback post-seek event") {
            DemuxReadEvent::TracksChanged(update) => {
                let replacement_tracks = update
                    .tracks
                    .into_iter()
                    .map(|track| (track.id, track.kind))
                    .collect::<Vec<_>>();
                assert_eq!(replacement_tracks, stable_tracks);
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
                assert_eq!(packet.pts, Duration::ZERO);
                break;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("fallback ended before exact-anchor packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
}

#[test]
fn separate_av_receipted_seek_commits_only_near_target_component_pair() {
    let video_segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                long_video_ts_segment(
                    segment_index
                        .saturating_mul(SEGMENT_SECONDS)
                        .saturating_mul(90_000),
                )
            })
            .collect::<Vec<_>>(),
    );
    let audio_segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                long_audio_ts_segment(
                    segment_index
                        .saturating_mul(SEGMENT_SECONDS)
                        .saturating_mul(90_000),
                    SEGMENT_SECONDS,
                )
            })
            .collect::<Vec<_>>(),
    );
    let video_playlist = Arc::new(component_playlist_text("video"));
    let audio_playlist = Arc::new(component_playlist_text("audio"));
    let server_video_segments = Arc::clone(&video_segments);
    let server_audio_segments = Arc::clone(&audio_segments);
    let server_video_playlist = Arc::clone(&video_playlist);
    let server_audio_playlist = Arc::clone(&audio_playlist);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Test audio\",\
                  DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\nvideo.m3u8\n",
            );
        }
        if request.request_line.contains("/video.m3u8") {
            return response("200 OK", &[], server_video_playlist.as_bytes());
        }
        if request.request_line.contains("/audio.m3u8") {
            return response("200 OK", &[], server_audio_playlist.as_bytes());
        }
        if let Some(response_bytes) =
            server_video_segments
                .iter()
                .enumerate()
                .find_map(|(segment_index, segment)| {
                    request
                        .request_line
                        .contains(&format!("/video-{segment_index}.ts"))
                        .then(|| response("200 OK", &[], segment))
                })
        {
            return response_bytes;
        }
        server_audio_segments
            .iter()
            .enumerate()
            .find_map(|(segment_index, segment)| {
                request
                    .request_line
                    .contains(&format!("/audio-{segment_index}.ts"))
                    .then(|| response("200 OK", &[], segment))
            })
            .unwrap_or_else(|| response("404 Not Found", &[], b""))
    });
    let opened = prepare_hls_vod_receipted(
        separate_av_request(&server),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare separate A/V HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("separate A/V receipted seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    let requests_before_seek = server.requests().len();

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue separate A/V late seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("separate A/V near-target seek must succeed: {receipt:?}");
    };
    let video_landing_position = result.actual_position.as_duration();
    assert!(
        (Duration::from_secs(60)..Duration::from_secs(61)).contains(&video_landing_position),
        "video RAP должен принадлежать target segment: {video_landing_position:?}"
    );

    let seek_request_lines = server
        .requests()
        .into_iter()
        .skip(requests_before_seek)
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/video-6.ts"))
    );
    assert!(
        seek_request_lines
            .iter()
            .any(|line| line.contains("/audio-6.ts"))
    );
    for skipped_segment in 1..6 {
        assert!(
            seek_request_lines.iter().all(|line| {
                !line.contains(&format!("/video-{skipped_segment}.ts"))
                    && !line.contains(&format!("/audio-{skipped_segment}.ts"))
            }),
            "separate A/V seek прочитал промежуточный segment: {seek_request_lines:?}"
        );
    }

    let mut landed_video = false;
    let mut landed_audio = false;
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !landed_video || !landed_audio {
        match next_ready_event(&mut *demuxer).expect("separate A/V post-seek event") {
            DemuxReadEvent::TracksChanged(update) => {
                let replacement_tracks = update
                    .tracks
                    .into_iter()
                    .map(|track| (track.id, track.kind))
                    .collect::<Vec<_>>();
                assert_eq!(replacement_tracks, stable_tracks);
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
                assert_eq!(packet.pts, video_landing_position);
                landed_video = true;
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => {
                assert!(packet.pts >= Duration::from_secs(60));
                landed_audio = true;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                panic!("separate A/V ended before both landing packets")
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(
            Instant::now() < deadline,
            "separate A/V landing packets timed out"
        );
    }
}
