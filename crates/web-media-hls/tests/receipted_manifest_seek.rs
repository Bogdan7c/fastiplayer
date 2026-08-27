//! Runtime-регрессия: поздний receipted seek стартует у manifest target, а не сканирует VOD с начала.

#[allow(dead_code)]
mod support;
mod receipted_manifest_seek {
    include!("receipted_manifest_seek/diagnostics.rs");
}
mod separate_av_cancellation {
    include!("receipted_manifest_seek/separate_av_cancellation.rs");
}

use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use demux_api::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, PacketKeyframe, TrackId, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, adaptive_context_without_completed_cache,
    demux_registry, large_muxed_ts_segment_with_early_landing, long_audio_ts_segment,
    long_interleaved_muxed_ts_segment, long_muxed_ts_segment, long_muxed_ts_segment_without_rap,
    long_video_ts_segment, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsComponentContainerIntent,
    HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides,
    HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest, HlsVodSeekLandingPolicy,
    SecretInlineMediaPlaylist, prepare_hls_vod_receipted,
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

/// Явно моделирует native HLS VOD; default helper намеренно остаётся legacy.
fn post_target_request(server: &TestServer, playlist: &str) -> HlsVodOpenRequest {
    inline_request(server, playlist)
        .with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap)
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
    let request = HlsVodOpenRequest {
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
    };
    request.with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap)
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
                if segment_index == 7 {
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
        post_target_request(&server, &playlist),
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
        Duration::from_secs(70)
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
            .any(|line| line.contains("/segment-7.ts")),
        "seek должен открыть первый post-target segment: {seek_request_lines:?}"
    );
    for skipped_segment in 1..7 {
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
                assert_eq!(packet.pts, Duration::from_secs(70));
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

/// Default HLS VOD path обязан сохранять containing-segment/decode-forward семантику.
#[test]
fn default_receipted_seek_keeps_containing_segment_decode_forward_semantics() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let segment_start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                long_muxed_ts_segment(segment_start_pts, SEGMENT_SECONDS)
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
    .expect("prepare default receipted HLS VOD");
    let seek_handle = opened.async_seek_handle().expect("default HLS seek handle");
    let mut demuxer = opened.into_demuxer();
    let _stable_tracks = initial_track_signature(&mut *demuxer);
    let requests_before_seek = server.requests().len();

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue default worker seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("default manifest seek must succeed: {receipt:?}");
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
    let containing_request_index = seek_request_lines
        .iter()
        .position(|line| line.contains("/segment-6.ts"))
        .expect("default seek обязан открыть containing segment");
    if let Some(next_request_index) = seek_request_lines
        .iter()
        .position(|line| line.contains("/segment-7.ts"))
    {
        assert!(
            containing_request_index < next_request_index,
            "parser read-ahead допустим только после containing selection: {seek_request_lines:?}"
        );
    }

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match next_ready_event(&mut *demuxer).expect("default post-seek event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
                assert_eq!(packet.pts, Duration::from_secs(60));
                break;
            }
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                panic!("default HLS ended before containing-segment landing packet")
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(
            Instant::now() < deadline,
            "default containing-segment landing packet timed out"
        );
    }
}

/// Failed B не отменяет committed source A и не лишает worker следующего retry C.
#[test]
fn failed_seek_after_committed_streaming_replacement_keeps_worker_retryable() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let segment_start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                long_muxed_ts_segment(segment_start_pts, SEGMENT_SECONDS)
            })
            .collect::<Vec<_>>(),
    );
    let server_segments = Arc::clone(&segments);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/segment-5.ts") {
            return response("503 Service Unavailable", &[], b"");
        }
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
        post_target_request(&server, &playlist_text()),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(1).expect("seek receipt bound")),
    )
    .expect("prepare retryable receipted HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("receipted HLS seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);

    for (request_id, target_seconds, expected_actual_seconds) in
        [(1, 65, Some(70)), (2, 45, None), (3, 75, Some(70))]
    {
        let requests_before_current_seek = server.requests().len();
        seek_handle
            .enqueue(
                ProgressiveSeekFence {
                    runtime_generation: seek_handle.runtime_generation(),
                    request_id: ProgressiveSeekRequestId::new(request_id),
                },
                DemuxSeekRequest::decode_point_before(Duration::from_secs(target_seconds)),
            )
            .expect("terminal failure не должен останавливать HLS seek worker");
        let receipt = wait_for_receipt(&seek_handle);
        assert_eq!(receipt.fence.request_id.value(), request_id);
        match (&receipt.outcome, expected_actual_seconds) {
            (ProgressiveAsyncSeekOutcome::Succeeded(result), Some(expected_actual_seconds)) => {
                assert_eq!(
                    result.actual_position.as_duration(),
                    Duration::from_secs(expected_actual_seconds),
                    "success обязан публиковать RAP реально выбранного post-target segment-а; receipt={receipt:?}; requests={:?}",
                    server.requests()
                );
            }
            (ProgressiveAsyncSeekOutcome::Failed, None) => {}
            _ => panic!(
                "controlled request {request_id} обязан сохранить terminal outcome: {receipt:?}"
            ),
        }
        if request_id == 2 {
            loop {
                match next_ready_event(&mut *demuxer)
                    .expect("failed B не должен отравить committed source A")
                {
                    DemuxReadEvent::EndOfStream => break,
                    DemuxReadEvent::TracksChanged(_)
                    | DemuxReadEvent::Packet(_)
                    | DemuxReadEvent::MediaMetadataChanged(_) => {}
                    DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
                }
            }
        }
        if request_id == 3 {
            assert_eq!(
                server.requests().len(),
                requests_before_current_seek,
                "inside-final seek должен переиспользовать proven exact anchor без несуществующего future GET"
            );
        }
    }

    let request_lines = server
        .requests()
        .into_iter()
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    assert!(
        request_lines
            .iter()
            .any(|line| line.contains("/segment-5.ts")),
        "ошибочный B обязан дойти до controlled transport failure: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.contains("/segment-4.ts")),
        "post-target B не должен ошибочно открывать containing segment: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .any(|line| line.contains("/segment-7.ts")),
        "retry C обязан сохранить proven final-segment source: {request_lines:?}"
    );

    loop {
        match next_ready_event(&mut *demuxer).expect("retry C post-seek event") {
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
                assert_eq!(packet.pts, Duration::from_secs(70));
                break;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("retry C ended before target packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
}

/// Accepted seek физически рвёт stalled old body, а failed replacement лениво открывает его с byte 0.
#[test]
fn accepted_seek_interrupts_stalled_body_and_failed_replacement_rolls_back_exact_segment() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let segment_start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                if segment_index == 0 {
                    large_muxed_ts_segment_with_early_landing(segment_start_pts)
                } else {
                    long_muxed_ts_segment(segment_start_pts, SEGMENT_SECONDS)
                }
            })
            .collect::<Vec<_>>(),
    );
    let first_body_prefix_ready = Arc::new((Mutex::new(false), Condvar::new()));
    let server_prefix_ready = Arc::clone(&first_body_prefix_ready);
    let first_body_dropped = Arc::new(AtomicBool::new(false));
    let server_body_dropped = Arc::clone(&first_body_dropped);
    let segment_zero_requests = Arc::new(AtomicUsize::new(0));
    let server_segment_zero_requests = Arc::clone(&segment_zero_requests);
    let server_segments = Arc::clone(&segments);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request.request_line.contains("/segment-5.ts") {
            stream
                .write_all(&response("503 Service Unavailable", &[], b""))
                .expect("write controlled replacement failure");
            return;
        }
        let Some((segment_index, segment)) =
            server_segments
                .iter()
                .enumerate()
                .find(|(segment_index, _)| {
                    request
                        .request_line
                        .contains(&format!("/segment-{segment_index}.ts"))
                })
        else {
            stream
                .write_all(&response("404 Not Found", &[], b""))
                .expect("write missing rollback fixture response");
            return;
        };
        if segment_index != 0 || server_segment_zero_requests.fetch_add(1, Ordering::AcqRel) > 0 {
            if let Err(error) = stream.write_all(&response("200 OK", &[], segment))
                && !matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                )
            {
                panic!("write complete rollback fixture segment: {error}");
            }
            return;
        }

        let prefix_bytes = (64 * 1_024).min(segment.len());
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            segment.len()
        );
        stream
            .write_all(headers.as_bytes())
            .expect("write stalled committed response headers");
        stream
            .write_all(&segment[..prefix_bytes])
            .expect("write stalled committed response prefix");
        stream.flush().expect("flush stalled committed prefix");
        let (ready_lock, ready_event) = &*server_prefix_ready;
        *ready_lock.lock().expect("stalled prefix state") = true;
        ready_event.notify_all();

        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("set stalled body close timeout");
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut probe = [0_u8; 1];
        while Instant::now() < deadline {
            match stream.read(&mut probe) {
                Ok(0) => {
                    server_body_dropped.store(true, Ordering::Release);
                    return;
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    server_body_dropped.store(true, Ordering::Release);
                    return;
                }
                Err(error) => panic!("observe stalled committed body drop: {error}"),
            }
        }
    });

    let mut request = post_target_request(&server, &playlist_text());
    request.policy.progressive_limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(4_096).expect("active-read event capacity"),
        NonZeroUsize::new(16 * 1_024 * 1_024).expect("active-read packet byte capacity"),
    );
    let opened = prepare_hls_vod_receipted(
        request,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(1).expect("seek receipt bound")),
    )
    .expect("prepare active-read rollback HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("active-read rollback seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    let (ready_lock, ready_event) = &*first_body_prefix_ready;
    let ready = ready_lock.lock().expect("stalled prefix state");
    let (ready, _) = ready_event
        .wait_timeout_while(ready, TEST_TIMEOUT, |ready| !*ready)
        .expect("wait stalled committed prefix");
    assert!(*ready, "committed response prefix должен быть отправлен");
    drop(ready);
    // После initial TracksChanged worker дочитывает replay-prefix и входит в контролируемый
    // pending body read. Fixture содержит только null-packet хвост, поэтому новых queue events нет.
    std::thread::sleep(Duration::from_millis(50));

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(45)),
        )
        .expect("enqueue seek while committed body stalls");
    let receipt = wait_for_receipt(&seek_handle);
    assert!(
        matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Failed),
        "controlled target failure не должен публиковать success: {receipt:?}"
    );
    assert!(
        first_body_dropped.load(Ordering::Acquire),
        "accepted seek обязан физически drop-нуть stalled old response до replacement GET"
    );

    let rollback_packet = loop {
        match next_ready_event(&mut *demuxer).expect("failed replacement rollback read") {
            DemuxReadEvent::TracksChanged(update) => {
                let rollback_tracks = update
                    .tracks
                    .into_iter()
                    .map(|track| (track.id, track.kind))
                    .collect::<Vec<_>>();
                assert_eq!(rollback_tracks, stable_tracks);
            }
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("rollback ended before first old-source packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    };
    assert_eq!(rollback_packet.pts, Duration::ZERO);
    assert_eq!(segment_zero_requests.load(Ordering::Acquire), 2);
    let request_lines = server
        .requests()
        .into_iter()
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    let failed_replacement_index = request_lines
        .iter()
        .position(|line| line.contains("/segment-5.ts"))
        .expect("controlled replacement request");
    assert!(
        request_lines
            .iter()
            .all(|line| !line.contains("/segment-4.ts")),
        "failed post-target replacement не должен обращаться к containing segment: {request_lines:?}"
    );
    let rollback_index = request_lines
        .iter()
        .rposition(|line| line.contains("/segment-0.ts"))
        .expect("rollback segment request");
    assert!(
        failed_replacement_index < rollback_index,
        "{request_lines:?}"
    );
}

/// Target RAP receipt не ждёт многомегабайтный хвост того же HTTP segment-а.
#[test]
fn receipted_seek_opens_and_lands_before_target_segment_body_completes() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let segment_start_pts = segment_index
                    .saturating_mul(SEGMENT_SECONDS)
                    .saturating_mul(90_000);
                if segment_index == 7 {
                    large_muxed_ts_segment_with_early_landing(segment_start_pts)
                } else {
                    long_muxed_ts_segment(segment_start_pts, SEGMENT_SECONDS)
                }
            })
            .collect::<Vec<_>>(),
    );
    let target_segment_bytes = segments[7].len();
    let target_bytes_written = Arc::new(AtomicUsize::new(0));
    let server_bytes_written = Arc::clone(&target_bytes_written);
    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let server_gate = Arc::clone(&gate);
    let server_segments = Arc::clone(&segments);
    let server = TestServer::start_streaming(move |_, request, stream| {
        let Some((_, segment)) = server_segments
            .iter()
            .enumerate()
            .find(|(segment_index, _)| {
                request
                    .request_line
                    .contains(&format!("/segment-{segment_index}.ts"))
            })
        else {
            stream
                .write_all(&response("404 Not Found", &[], b""))
                .expect("write missing HLS fixture response");
            return;
        };
        if !request.request_line.contains("/segment-7.ts") {
            stream
                .write_all(&response("200 OK", &[], segment))
                .expect("write ordinary HLS fixture segment");
            return;
        }
        let prefix_bytes = (64 * 1_024).min(segment.len());
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            segment.len()
        );
        stream
            .write_all(headers.as_bytes())
            .expect("write gated HLS response headers");
        stream
            .write_all(&segment[..prefix_bytes])
            .expect("write gated HLS response prefix");
        stream.flush().expect("flush gated HLS response prefix");
        server_bytes_written.store(prefix_bytes, Ordering::Release);
        let (lock, ready) = &*server_gate;
        let mut state = lock.lock().expect("gated HLS state");
        state.0 = true;
        ready.notify_all();
        let (state_after_wait, _) = ready
            .wait_timeout_while(state, TEST_TIMEOUT, |state| !state.1)
            .expect("wait gated HLS tail release");
        if state_after_wait.1 {
            match stream.write_all(&segment[prefix_bytes..]) {
                Ok(()) => server_bytes_written.store(segment.len(), Ordering::Release),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) => {}
                Err(error) => panic!("write gated HLS response tail: {error}"),
            }
        }
    });
    let opened = prepare_hls_vod_receipted(
        post_target_request(&server, &playlist_text()),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare streaming HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("streaming HLS seek handle");
    let mut demuxer = opened.into_demuxer();
    let _stable_tracks = initial_track_signature(&mut *demuxer);
    let requests_before_seek = server.requests().len();

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue streaming worker seek");
    let (gate_lock, gate_ready) = &*gate;
    let state = gate_lock.lock().expect("gated HLS state");
    let (state, _) = gate_ready
        .wait_timeout_while(state, TEST_TIMEOUT, |state| !state.0)
        .expect("wait gated HLS prefix");
    assert!(
        state.0,
        "target segment prefix должен быть физически отправлен"
    );
    drop(state);

    let receipt_deadline = Instant::now() + TEST_TIMEOUT;
    let receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        if Instant::now() >= receipt_deadline {
            let mut state = gate_lock.lock().expect("release timed-out HLS gate");
            state.1 = true;
            gate_ready.notify_all();
            panic!("HLS receipt не должен ждать gated segment tail");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(
        matches!(&receipt.outcome, ProgressiveAsyncSeekOutcome::Succeeded(_)),
        "streaming receipt должен завершиться по prefix: {receipt:?}; requests={:?}",
        server.requests()
    );
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = &receipt.outcome else {
        unreachable!("outcome проверен выше")
    };
    assert_eq!(
        result.actual_position.as_duration(),
        Duration::from_secs(70)
    );
    assert_eq!(target_bytes_written.load(Ordering::Acquire), 64 * 1_024);
    assert!(target_bytes_written.load(Ordering::Acquire) < target_segment_bytes);
    assert_eq!(
        server
            .requests()
            .iter()
            .skip(requests_before_seek)
            .filter(|request| request.request_line.contains("/segment-7.ts"))
            .count(),
        1,
        "target segment должен иметь единственный HTTP request"
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
            .all(|line| !line.contains("/segment-6.ts")),
        "streaming seek не должен открывать containing segment: {seek_request_lines:?}"
    );

    let mut state = gate_lock.lock().expect("release gated HLS tail");
    state.1 = true;
    gate_ready.notify_all();
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
        post_target_request(&server, &playlist_text()),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare fallback HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("receipted fallback seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    let requests_before_seek = server.requests();
    let request_count_before_seek = requests_before_seek.len();
    let segment_zero_requests_before_seek = requests_before_seek
        .iter()
        .filter(|request| request.request_line.contains("/segment-0.ts"))
        .count();

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

    let requests_after_seek = server.requests();
    let seek_request_lines = requests_after_seek
        .iter()
        .skip(request_count_before_seek)
        .map(|request| request.request_line.clone())
        .collect::<Vec<_>>();
    let post_target_candidate_index = seek_request_lines
        .iter()
        .position(|line| line.contains("/segment-7.ts"))
        .expect("post-target candidate должен проверяться первым");
    let containing_candidate_index = seek_request_lines
        .iter()
        .position(|line| line.contains("/segment-6.ts"))
        .expect("legacy fallback должен проверить containing segment");
    let previous_candidate_index = seek_request_lines
        .iter()
        .position(|line| line.contains("/segment-5.ts"))
        .expect("legacy fallback должен проверить previous same-epoch segment");
    assert!(post_target_candidate_index < containing_candidate_index);
    assert!(containing_candidate_index < previous_candidate_index);
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
    assert_eq!(
        requests_after_seek
            .iter()
            .filter(|request| request.request_line.contains("/segment-0.ts"))
            .count(),
        segment_zero_requests_before_seek,
        "legacy exact-anchor restart должен переиспользовать completed VOD resource без второго HTTP request"
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
fn separate_av_receipted_seek_commits_exact_boundary_component_pair() {
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
            DemuxSeekRequest::decode_point_before(Duration::from_secs(60)),
        )
        .expect("enqueue separate A/V exact-boundary seek");
    let receipt = wait_for_receipt(&seek_handle);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("separate A/V exact-boundary seek must succeed: {receipt:?}");
    };
    let video_landing_position = result.actual_position.as_duration();
    assert!(
        video_landing_position > Duration::from_secs(60)
            && video_landing_position < Duration::from_secs(61),
        "receipt должен сохранить post-target PTS target-segment RAP: {video_landing_position:?}"
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
