//! Deterministic regression ровно одного fresh manifest-resource restart-а.

#[allow(dead_code)]
mod support;

use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use demux_api::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_muxed_ts_segment,
    long_muxed_ts_segment_without_rap, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, HlsVodSeekLandingPolicy,
    SecretInlineMediaPlaylist, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const SEGMENT_SECONDS: u64 = 10;
const SEGMENT_COUNT: u64 = 8;
const TARGET_SEGMENT_INDEX: u64 = 7;
const CONTAINING_SEGMENT_INDEX: u64 = 6;
const SUPERSEDING_SEGMENT_INDEX: u64 = 4;

/// Собирает native-like opt-in request для deterministic post-target retry/cancellation checks.
fn post_target_request(server: &TestServer) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let selected_url = server.target("/transient-manifest-retry.m3u8");
    let segment_lines = (0..SEGMENT_COUNT)
        .map(|segment_index| format!("#EXTINF:{SEGMENT_SECONDS},\nsegment-{segment_index}.ts\n"))
        .collect::<String>();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}\n{segment_lines}#EXT-X-ENDLIST\n"
    );
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
        manifest: HlsManifestInput::InlineMedia {
            selected_url,
            playlist: SecretInlineMediaPlaylist::new(&playlist),
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
    .with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap)
}

/// Добавляет boundary discontinuity непосредственно перед целью старого seek-а.
///
/// Так cancellation-тест ниже проверяет не только закрытие partial response, но и
/// fencing уже выбранной новой HLS epoch: после supersede она не имеет права
/// опубликовать ни receipt, ни packet поверх более свежего backward seek-а.
fn discontinuous_post_target_request(server: &TestServer) -> HlsVodOpenRequest {
    let mut request = post_target_request(server);
    let selected_url = server.target("/transient-manifest-retry.m3u8");
    let segment_lines = (0..SEGMENT_COUNT)
        .map(|segment_index| {
            let discontinuity = if segment_index == TARGET_SEGMENT_INDEX {
                "#EXT-X-DISCONTINUITY\n"
            } else {
                ""
            };
            format!("{discontinuity}#EXTINF:{SEGMENT_SECONDS},\nsegment-{segment_index}.ts\n")
        })
        .collect::<String>();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}\n{segment_lines}#EXT-X-ENDLIST\n"
    );
    request.manifest = HlsManifestInput::InlineMedia {
        selected_url,
        playlist: SecretInlineMediaPlaylist::new(&playlist),
    };
    request
}

/// Ждёт readiness event без превращения `TemporarilyUnavailable` в blocking API.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS readiness timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return Ok(event),
        }
    }
}

/// Открывает worker, принимает initial tracks и выполняет public near-target seek.
fn run_seek(
    server: &TestServer,
    request_id: u64,
) -> (
    Box<dyn Demuxer + Send>,
    demux_api::ProgressiveAsyncSeekReceipt,
) {
    let (seek_handle, demuxer) = prepared_worker(server);
    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(request_id),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue manifest seek");
    let receipt = wait_for_receipt(&seek_handle);
    (demuxer, receipt)
}

/// Возвращает готовые public seek handle и demuxer после initial track publication.
fn prepared_worker(
    server: &TestServer,
) -> (
    demux_api::ProgressiveAsyncSeekHandle,
    Box<dyn Demuxer + Send>,
) {
    prepared_worker_from_request(post_target_request(server))
}

/// Поднимает production worker с конкретным manifest fixture и возвращает его public boundaries.
fn prepared_worker_from_request(
    request: HlsVodOpenRequest,
) -> (
    demux_api::ProgressiveAsyncSeekHandle,
    Box<dyn Demuxer + Send>,
) {
    let opened = prepare_hls_vod_receipted(
        request,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare transient-retry HLS VOD");
    let seek_handle = opened
        .async_seek_handle()
        .expect("transient-retry seek handle");
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(&mut *demuxer).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    (seek_handle, demuxer)
}

/// Поллит authoritative receipt с deterministic deadline.
fn wait_for_receipt(
    seek_handle: &demux_api::ProgressiveAsyncSeekHandle,
) -> demux_api::ProgressiveAsyncSeekReceipt {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            return receipt;
        }
        assert!(Instant::now() < deadline, "manifest seek receipt timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Отдаёт syntactically valid prefix, но закрывает body раньше объявленной длины.
fn write_truncated_body(stream: &mut std::net::TcpStream, prefix: &[u8]) {
    let declared_bytes = prefix.len().saturating_add(188);
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {declared_bytes}\r\nConnection: close\r\n\r\n"
    )
    .expect("write truncated response headers");
    stream
        .write_all(prefix)
        .expect("write truncated response prefix");
    stream.flush().expect("flush truncated response prefix");
}

/// Возвращает сегмент по request line либо deterministic 404.
fn ordinary_segment_response(
    request_line: &str,
    segments: &[Vec<u8>],
    target_response: &[u8],
) -> Vec<u8> {
    if request_line.contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts")) {
        return response("200 OK", &[], target_response);
    }
    segments
        .iter()
        .enumerate()
        .find_map(|(segment_index, segment)| {
            request_line
                .contains(&format!("/segment-{segment_index}.ts"))
                .then(|| response("200 OK", &[], segment))
        })
        .unwrap_or_else(|| response("404 Not Found", &[], b""))
}

/// Post-target policy не должна обращаться к containing segment-у исходного target `65 s`.
fn assert_no_containing_segment_request(server: &TestServer) {
    let request_lines = server
        .requests()
        .into_iter()
        .map(|request| request.request_line)
        .collect::<Vec<_>>();
    assert!(
        request_lines
            .iter()
            .all(|line| !line.contains(&format!("/segment-{CONTAINING_SEGMENT_INDEX}.ts"))),
        "post-target seek не должен открывать containing segment: {request_lines:?}"
    );
}

/// Читает первый committed video packet после authoritative receipt-а.
fn first_post_seek_video_packet(demuxer: &mut dyn Demuxer) -> Packet {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match next_ready_event(demuxer).expect("post-seek event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => return packet,
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("stream ended before target video packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(Instant::now() < deadline, "target video packet timed out");
    }
}

/// Partial attempt A полностью уничтожается; attempt B начинает source с byte zero.
#[test]
fn transient_partial_body_restarts_whole_candidate_once_without_splice() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
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
    let complete_target = Arc::new(segments[TARGET_SEGMENT_INDEX as usize].clone());
    let incompatible_prefix = Arc::new(long_muxed_ts_segment_without_rap(
        1_000_u64.saturating_mul(90_000),
        2,
    ));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let server_segments = Arc::clone(&segments);
    let server_target = Arc::clone(&complete_target);
    let server_prefix = Arc::clone(&incompatible_prefix);
    let server_attempts = Arc::clone(&target_attempts);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request
            .request_line
            .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts"))
        {
            let attempt = server_attempts.fetch_add(1, Ordering::AcqRel);
            if attempt == 0 {
                write_truncated_body(stream, &server_prefix);
            } else {
                stream
                    .write_all(&response("200 OK", &[], &server_target))
                    .expect("write complete restarted target");
            }
            return;
        }
        stream
            .write_all(&ordinary_segment_response(
                &request.request_line,
                &server_segments,
                &server_target,
            ))
            .expect("write ordinary segment");
    });

    let (mut demuxer, receipt) = run_seek(&server, 1);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("single fresh restart должен успешно доказать target: {receipt:?}");
    };
    assert_eq!(
        result.actual_position.as_duration(),
        Duration::from_secs(70)
    );
    assert_eq!(
        target_attempts.load(Ordering::Acquire),
        2,
        "второй physical request доказывает, что partial A не попал в completed cache"
    );
    assert_no_containing_segment_request(&server);
    let landing = first_post_seek_video_packet(&mut *demuxer);
    let landing_offset = landing
        .byte_offset
        .expect("committed packet должен сохранять source provenance");
    assert!(
        landing_offset < u64::try_from(incompatible_prefix.len()).expect("prefix length"),
        "fresh parser обязан считать byte offset от B, а не от A+B splice: {landing_offset}"
    );
}

/// Повторная transient body failure исчерпывает budget ровно после второго request-а.
#[test]
fn persistent_transient_body_failure_is_bounded_to_two_requests() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
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
    let partial = Arc::new(long_muxed_ts_segment_without_rap(90_000_000, 2));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let server_segments = Arc::clone(&segments);
    let server_partial = Arc::clone(&partial);
    let server_attempts = Arc::clone(&target_attempts);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request
            .request_line
            .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts"))
        {
            server_attempts.fetch_add(1, Ordering::AcqRel);
            write_truncated_body(stream, &server_partial);
            return;
        }
        stream
            .write_all(&ordinary_segment_response(
                &request.request_line,
                &server_segments,
                &server_segments[TARGET_SEGMENT_INDEX as usize],
            ))
            .expect("write ordinary persistent-failure segment");
    });

    let (_demuxer, receipt) = run_seek(&server, 1);
    assert!(
        matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Failed),
        "persistent transient failure должна остаться terminal: {receipt:?}"
    );
    assert_eq!(target_attempts.load(Ordering::Acquire), 2);
    assert_no_containing_segment_request(&server);
}

/// Нормальный body не создаёт speculative second request.
#[test]
fn successful_body_uses_single_request() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
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
        ordinary_segment_response(
            &request.request_line,
            &server_segments,
            &server_segments[TARGET_SEGMENT_INDEX as usize],
        )
    });

    let (_demuxer, receipt) = run_seek(&server, 1);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("полный post-target body должен успешно завершить seek: {receipt:?}");
    };
    assert_eq!(
        result.actual_position.as_duration(),
        Duration::from_secs(70)
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request
                .request_line
                .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts")))
            .count(),
        1
    );
    assert_no_containing_segment_request(&server);
}

/// Permanent HTTP status не попадает в body-stage restart policy.
#[test]
fn fatal_http_status_is_not_retried() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
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
        if request
            .request_line
            .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts"))
        {
            response("404 Not Found", &[], b"")
        } else {
            ordinary_segment_response(
                &request.request_line,
                &server_segments,
                &server_segments[TARGET_SEGMENT_INDEX as usize],
            )
        }
    });

    let (_demuxer, receipt) = run_seek(&server, 1);
    assert!(matches!(
        receipt.outcome,
        ProgressiveAsyncSeekOutcome::Failed
    ));
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request
                .request_line
                .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts")))
            .count(),
        1
    );
    assert_no_containing_segment_request(&server);
}

/// Supersede закрывает partial response discontinuous epoch и не даёт ей опубликоваться.
#[test]
fn cancellation_of_discontinuous_partial_body_prevents_stale_publication_and_restart() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
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
    let partial = Arc::new(long_muxed_ts_segment_without_rap(90_000_000, 2));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let (prefix_sender, prefix_receiver) = mpsc::sync_channel(1);
    let (release_sender, release_receiver) = mpsc::sync_channel(1);
    let release_receiver = Arc::new(std::sync::Mutex::new(release_receiver));
    let (attempts_before_latest_sender, attempts_before_latest_receiver) = mpsc::sync_channel(1);
    let server_segments = Arc::clone(&segments);
    let server_partial = Arc::clone(&partial);
    let server_attempts = Arc::clone(&target_attempts);
    let server_release = Arc::clone(&release_receiver);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request
            .request_line
            .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts"))
        {
            let attempt = server_attempts.fetch_add(1, Ordering::AcqRel);
            if attempt > 0 {
                write_truncated_body(stream, &server_partial);
                return;
            }
            let declared_bytes = server_partial.len().saturating_add(188);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {declared_bytes}\r\nConnection: close\r\n\r\n"
            )
            .expect("write cancellable response headers");
            stream
                .write_all(&server_partial)
                .expect("write cancellable response prefix");
            stream.flush().expect("flush cancellable response prefix");
            prefix_sender.send(()).expect("publish prefix delivery");
            server_release
                .lock()
                .expect("release receiver lock")
                .recv_timeout(TEST_TIMEOUT)
                .expect("wait supersede before closing partial response");
            return;
        }
        if request
            .request_line
            .contains(&format!("/segment-{SUPERSEDING_SEGMENT_INDEX}.ts"))
        {
            // Worker обрабатывает seek-и последовательно: любой запрещённый restart
            // старого segment-7 обязан случиться до начала latest segment-4 request-а.
            attempts_before_latest_sender
                .send(server_attempts.load(Ordering::Acquire))
                .expect("publish old target attempts before latest request");
        }
        stream
            .write_all(&ordinary_segment_response(
                &request.request_line,
                &server_segments,
                &server_segments[TARGET_SEGMENT_INDEX as usize],
            ))
            .expect("write supersede fixture segment");
    });
    let (seek_handle, mut demuxer) =
        prepared_worker_from_request(discontinuous_post_target_request(&server));

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(65)),
        )
        .expect("enqueue cancellable target seek");
    prefix_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("target prefix must reach client");
    assert_no_containing_segment_request(&server);
    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(2),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(35)),
        )
        .expect("enqueue superseding seek");
    release_sender
        .send(())
        .expect("release cancelled partial response");
    let attempts_before_latest = attempts_before_latest_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("latest backward target request must reach server");
    assert_eq!(
        attempts_before_latest,
        1,
        "cancelled old target нельзя рестартовать до latest seek: requests={:?}",
        server.requests()
    );

    let first_receipt = wait_for_receipt(&seek_handle);
    let second_receipt = wait_for_receipt(&seek_handle);
    let receipts = [first_receipt, second_receipt];
    let superseded_receipt = receipts
        .iter()
        .find(|receipt| receipt.fence.request_id.value() == 1)
        .unwrap_or_else(|| panic!("receipt старого discontinuous seek потерян: {receipts:?}"));
    assert!(
        matches!(
            &superseded_receipt.outcome,
            ProgressiveAsyncSeekOutcome::Superseded
        ),
        "старый discontinuous seek обязан завершиться именно как superseded: {receipts:?}"
    );
    let latest_result = receipts
        .iter()
        .find_map(|receipt| match &receipt.outcome {
            ProgressiveAsyncSeekOutcome::Succeeded(result)
                if receipt.fence.request_id.value() == 2 =>
            {
                Some(result)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "новейший backward seek обязан завершиться успешно: {receipts:?}; requests={:?}",
                server.requests()
            )
        });
    assert_eq!(
        latest_result.actual_position.as_duration(),
        Duration::from_secs(40)
    );
    let presented_packet = first_post_seek_video_packet(&mut *demuxer);
    assert_eq!(
        presented_packet.pts,
        Duration::from_secs(40),
        "superseded discontinuous epoch не должна публиковать packet старого target-а"
    );
}
