#[allow(dead_code)]
mod support;

use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use demux_api::ProgressiveAsyncSeekLimits;
use media_core::{DemuxReadEvent, Demuxer, MediaTime, PacketKeyframe, TrackKind};
use source_core::CancellationToken;
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsInitialPositionProofCapability, HlsInitialPositionProofTakeOutcome,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, HlsVodRestoreFallbackReason,
    HlsVodSeekLandingPolicy, HlsVodStartDisposition, HlsVodStartIntent, SecretInlineMediaPlaylist,
    prepare_hls_vod, prepare_hls_vod_receipted_at_start,
};
use web_media_transport_api::SourceGeneration;

use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_muxed_ts_segment_without_rap,
    muxed_fmp4, muxed_ts_segment_with_early_landing, open_policy, response,
};

const READY_TIMEOUT: Duration = Duration::from_secs(5);

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn inline_request(server: &TestServer, playlist: &str) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(41);
    let target = server.target("/target-aware.m3u8");
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
            main: HlsContainerEvidence::ContentProbe,
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn fetched_request(server: &TestServer, manifest_path: &str) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(41);
    let target = server.target(manifest_path);
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

fn async_seek_limits() -> ProgressiveAsyncSeekLimits {
    ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek request capacity"))
}

fn next_ready_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let event = demuxer.next_event()?;
        if !matches!(event, DemuxReadEvent::TemporarilyUnavailable(_)) {
            return Ok(event);
        }
        assert!(
            Instant::now() < deadline,
            "HLS initial open readiness timeout"
        );
        thread::yield_now();
    }
}

fn request_count(server: &TestServer, path: &str) -> usize {
    let request_prefix = format!("GET {path} ");
    server
        .requests()
        .iter()
        .filter(|request| request.request_line.starts_with(&request_prefix))
        .count()
}

fn deferred_proof_port(
    opened: &web_media_hls::HlsVodOpenResult,
) -> web_media_hls::HlsInitialPositionProofPort {
    match opened.initial_position_proof() {
        HlsInitialPositionProofCapability::Deferred(port) => port,
        HlsInitialPositionProofCapability::NotRequested => {
            panic!("restore open must expose deferred initial-position proof")
        }
    }
}

#[test]
fn deferred_proof_remains_pending_until_actual_anchor_is_proven() {
    let segment = Arc::new(muxed_ts_segment_with_early_landing(1_800_000));
    let worker_gate = Arc::new(Barrier::new(2));
    let server = TestServer::start({
        let segment = Arc::clone(&segment);
        let worker_gate = Arc::clone(&worker_gate);
        move |_, request| {
            assert!(request.request_line.starts_with("GET /target.ts "));
            worker_gate.wait();
            worker_gate.wait();
            response("200 OK", &[], segment.as_slice())
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nunused.ts\n#EXTINF:10,\nunused-2.ts\n#EXTINF:10,\ntarget.ts\n#EXT-X-ENDLIST\n";
    let mut request = inline_request(&server, playlist);
    request.containers.main = HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream);
    let opened = prepare_hls_vod_receipted_at_start(
        request,
        async_seek_limits(),
        HlsVodStartIntent::Restore(MediaTime::from_secs(25)),
    )
    .expect("deferred restore prepare");
    let proof_port = deferred_proof_port(&opened);
    worker_gate.wait();
    assert_eq!(
        proof_port.take_for_generation(SourceGeneration::new(41)),
        HlsInitialPositionProofTakeOutcome::Pending
    );
    worker_gate.wait();

    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("deferred topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let HlsInitialPositionProofTakeOutcome::Ready(proof) =
        proof_port.take_for_generation(SourceGeneration::new(41))
    else {
        panic!("proof must become ready only after exact anchor proof");
    };
    assert_eq!(proof.target_position(), MediaTime::from_secs(25));
    assert_eq!(
        proof.demux_seek_result().requested_position,
        MediaTime::from_secs(25)
    );
    assert!(proof.demux_seek_result().actual_position <= proof.target_position());
}

#[test]
fn restore_opens_containing_ts_once_and_continues_the_probed_demuxer() {
    let segment_zero = Arc::new(muxed_ts_segment_with_early_landing(0));
    let segment_one = Arc::new(muxed_ts_segment_with_early_landing(900_000));
    let segment_two = Arc::new(muxed_ts_segment_with_early_landing(1_800_000));
    let server = TestServer::start({
        let segment_zero = Arc::clone(&segment_zero);
        let segment_one = Arc::clone(&segment_one);
        let segment_two = Arc::clone(&segment_two);
        move |_, request| {
            let body = if request.request_line.starts_with("GET /segment-0.ts ") {
                segment_zero.as_slice()
            } else if request.request_line.starts_with("GET /segment-1.ts ") {
                segment_one.as_slice()
            } else if request.request_line.starts_with("GET /segment-2.ts ") {
                segment_two.as_slice()
            } else {
                panic!("unexpected target-aware request: {}", request.request_line);
            };
            response("200 OK", &[], body)
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment-0.ts\n#EXTINF:10,\nsegment-1.ts\n#EXTINF:10,\nsegment-2.ts\n#EXT-X-ENDLIST\n";

    let opened = prepare_hls_vod_receipted_at_start(
        inline_request(&server, playlist),
        async_seek_limits(),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_secs(25)),
    )
    .expect("target-aware restore prepare");
    assert_eq!(
        opened.start_disposition(),
        HlsVodStartDisposition::RestoreRequested {
            target_position: MediaTime::from_secs(25),
        }
    );
    let proof_port = deferred_proof_port(&opened);
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("restore topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let DemuxReadEvent::Packet(first_video) =
        next_ready_event(demuxer.as_mut()).expect("restore landing packet")
    else {
        panic!("restore must continue the content-probed demuxer with a packet");
    };
    assert_eq!(first_video.kind, TrackKind::Video);
    assert_eq!(first_video.keyframe, PacketKeyframe::Keyframe);
    assert!(first_video.pts >= Duration::from_secs(20));
    assert_eq!(
        proof_port.take_for_generation(SourceGeneration::new(42)),
        HlsInitialPositionProofTakeOutcome::StaleGeneration
    );
    let HlsInitialPositionProofTakeOutcome::Ready(proof) =
        proof_port.take_for_generation(SourceGeneration::new(41))
    else {
        panic!("matching generation must receive exact initial proof");
    };
    assert_eq!(proof.generation(), SourceGeneration::new(41));
    assert_eq!(proof.target_position(), MediaTime::from_secs(25));
    assert_eq!(
        proof.demux_seek_result().requested_position,
        MediaTime::from_secs(25)
    );
    assert_eq!(
        proof.demux_seek_result().actual_position.as_duration(),
        first_video.pts
    );
    assert_eq!(
        proof_port.take_for_generation(SourceGeneration::new(41)),
        HlsInitialPositionProofTakeOutcome::AlreadyTaken
    );
    assert_eq!(request_count(&server, "/segment-2.ts"), 1);
    assert_eq!(request_count(&server, "/segment-0.ts"), 0);
    assert_eq!(request_count(&server, "/segment-1.ts"), 0);
}

#[test]
fn restore_inside_segment_prefers_first_post_target_manifest_boundary() {
    let containing_segment = Arc::new(muxed_ts_segment_with_early_landing(1_800_000));
    let post_target_segment = Arc::new(muxed_ts_segment_with_early_landing(2_700_000));
    let server = TestServer::start({
        let containing_segment = Arc::clone(&containing_segment);
        let post_target_segment = Arc::clone(&post_target_segment);
        move |_, request| {
            let body = if request.request_line.starts_with("GET /segment-2.ts ") {
                containing_segment.as_slice()
            } else if request.request_line.starts_with("GET /segment-3.ts ") {
                post_target_segment.as_slice()
            } else {
                panic!(
                    "unexpected post-target restore request: {}",
                    request.request_line
                );
            };
            response("200 OK", &[], body)
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment-0.ts\n#EXTINF:10,\nsegment-1.ts\n#EXTINF:10,\nsegment-2.ts\n#EXTINF:10,\nsegment-3.ts\n#EXT-X-ENDLIST\n";
    let requested_position = MediaTime::from_secs(25);

    let opened = prepare_hls_vod_receipted_at_start(
        inline_request(&server, playlist)
            .with_seek_landing_policy(HlsVodSeekLandingPolicy::PreferPostTargetRap),
        async_seek_limits(),
        HlsVodStartIntent::RestoreOrBeginning(requested_position),
    )
    .expect("post-target restore prepare");
    let proof_port = deferred_proof_port(&opened);
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("post-target topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let DemuxReadEvent::Packet(first_video) =
        next_ready_event(demuxer.as_mut()).expect("post-target landing packet")
    else {
        panic!("post-target restore must replay its proven video RAP");
    };
    let HlsInitialPositionProofTakeOutcome::Ready(proof) =
        proof_port.take_for_generation(SourceGeneration::new(41))
    else {
        panic!("matching generation must receive post-target proof");
    };

    assert_eq!(first_video.kind, TrackKind::Video);
    assert_eq!(first_video.keyframe, PacketKeyframe::Keyframe);
    assert!(first_video.pts >= Duration::from_secs(30));
    assert_eq!(proof.target_position(), requested_position);
    assert_eq!(
        proof.demux_seek_result().requested_position,
        requested_position
    );
    assert_eq!(
        proof.demux_seek_result().actual_position.as_duration(),
        first_video.pts
    );
    assert_eq!(request_count(&server, "/segment-3.ts"), 1);
    assert_eq!(request_count(&server, "/segment-2.ts"), 0);
}

#[test]
fn beginning_keeps_first_segment_and_reuses_its_content_probe() {
    let segment_zero = Arc::new(muxed_ts_segment_with_early_landing(0));
    let segment_one = Arc::new(muxed_ts_segment_with_early_landing(900_000));
    let server = TestServer::start({
        let segment_zero = Arc::clone(&segment_zero);
        let segment_one = Arc::clone(&segment_one);
        move |_, request| {
            let body = if request.request_line.starts_with("GET /segment-0.ts ") {
                segment_zero.as_slice()
            } else if request.request_line.starts_with("GET /segment-1.ts ") {
                segment_one.as_slice()
            } else {
                panic!("unexpected beginning request: {}", request.request_line);
            };
            response("200 OK", &[], body)
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment-0.ts\n#EXTINF:10,\nsegment-1.ts\n#EXT-X-ENDLIST\n";

    let opened = prepare_hls_vod(inline_request(&server, playlist)).expect("beginning prepare");
    assert_eq!(
        opened.start_disposition(),
        HlsVodStartDisposition::BeginningRequested
    );
    assert!(matches!(
        opened.initial_position_proof(),
        HlsInitialPositionProofCapability::NotRequested
    ));
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("beginning topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert_eq!(request_count(&server, "/segment-0.ts"), 1);
}

#[test]
fn containing_without_rap_falls_back_to_previous_without_publishing_failed_candidate() {
    let previous = Arc::new(muxed_ts_segment_with_early_landing(900_000));
    let containing = Arc::new(long_muxed_ts_segment_without_rap(1_800_000, 10));
    let server = TestServer::start({
        let previous = Arc::clone(&previous);
        let containing = Arc::clone(&containing);
        move |_, request| {
            let body = if request.request_line.starts_with("GET /previous.ts ") {
                previous.as_slice()
            } else if request.request_line.starts_with("GET /containing.ts ") {
                containing.as_slice()
            } else {
                panic!("unexpected fallback request: {}", request.request_line);
            };
            response("200 OK", &[], body)
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nunused.ts\n#EXTINF:10,\nprevious.ts\n#EXTINF:10,\ncontaining.ts\n#EXT-X-ENDLIST\n";

    let opened = prepare_hls_vod_receipted_at_start(
        inline_request(&server, playlist),
        async_seek_limits(),
        HlsVodStartIntent::Restore(MediaTime::from_secs(25)),
    )
    .expect("fallback restore prepare");
    let proof_port = deferred_proof_port(&opened);
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("fallback topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let DemuxReadEvent::Packet(packet) = next_ready_event(demuxer.as_mut()).expect("fallback RAP")
    else {
        panic!("failed containing candidate must not publish a lifecycle event");
    };
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
    assert!(packet.pts < Duration::from_secs(20));
    let HlsInitialPositionProofTakeOutcome::Ready(proof) =
        proof_port.take_for_generation(SourceGeneration::new(41))
    else {
        panic!("successful previous candidate must publish one final proof");
    };
    assert_eq!(
        proof.demux_seek_result().actual_position.as_duration(),
        packet.pts
    );
    assert_eq!(request_count(&server, "/containing.ts"), 1);
    assert_eq!(request_count(&server, "/previous.ts"), 1);
    assert_eq!(request_count(&server, "/unused.ts"), 0);
}

#[test]
fn terminal_candidate_failure_settles_proof_without_fabricated_position() {
    let failed_segment = Arc::new(long_muxed_ts_segment_without_rap(900_000, 10));
    let server = TestServer::start({
        let failed_segment = Arc::clone(&failed_segment);
        move |_, request| {
            assert!(
                request.request_line.starts_with("GET /previous.ts ")
                    || request.request_line.starts_with("GET /containing.ts ")
            );
            response("200 OK", &[], failed_segment.as_slice())
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nunused.ts\n#EXTINF:10,\nprevious.ts\n#EXTINF:10,\ncontaining.ts\n#EXT-X-ENDLIST\n";
    let mut request = inline_request(&server, playlist);
    request.containers.main = HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream);
    let opened = prepare_hls_vod_receipted_at_start(
        request,
        async_seek_limits(),
        HlsVodStartIntent::Restore(MediaTime::from_secs(25)),
    )
    .expect("terminal candidate failure remains deferred");
    let proof_port = deferred_proof_port(&opened);
    let mut demuxer = opened.into_demuxer();
    assert!(next_ready_event(demuxer.as_mut()).is_err());
    assert_eq!(
        proof_port.take_for_generation(SourceGeneration::new(41)),
        HlsInitialPositionProofTakeOutcome::Failed
    );
}

#[test]
fn fmp4_uses_one_bounded_ts_probe_then_one_finite_fallback() {
    let (mut padded_initialization, first_media, _) = muxed_fmp4();
    let free_box_size = 512 * 1_024 - padded_initialization.len();
    padded_initialization.extend_from_slice(&(free_box_size as u32).to_be_bytes());
    padded_initialization.extend_from_slice(b"free");
    padded_initialization.resize(512 * 1_024, 0);
    let padded_initialization = Arc::new(padded_initialization);
    let first_media = Arc::new(first_media);
    let first_attempt_body_bytes = Arc::new(AtomicUsize::new(0));
    let server = TestServer::start_streaming({
        let padded_initialization = Arc::clone(&padded_initialization);
        let first_media = Arc::clone(&first_media);
        let first_attempt_body_bytes = Arc::clone(&first_attempt_body_bytes);
        move |request_index, request, stream| {
            let response_bytes = if request.request_line.starts_with("GET /init.mp4 ") {
                response("200 OK", &[], padded_initialization.as_slice())
            } else if request.request_line.starts_with("GET /segment.m4s ") {
                response("200 OK", &[], first_media.as_slice())
            } else {
                panic!("unexpected fMP4 request: {}", request.request_line);
            };
            if request_index == 0 {
                let header_end = response_bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4)
                    .expect("HTTP header boundary");
                stream
                    .write_all(&response_bytes[..header_end])
                    .expect("write fMP4 headers");
                for body_chunk in response_bytes[header_end..].chunks(1_024) {
                    if stream.write_all(body_chunk).is_err() {
                        break;
                    }
                    first_attempt_body_bytes.fetch_add(body_chunk.len(), Ordering::AcqRel);
                    thread::yield_now();
                }
            } else {
                stream
                    .write_all(&response_bytes)
                    .expect("write finite fMP4 fallback");
            }
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\nsegment.m4s\n#EXT-X-ENDLIST\n";

    let opened = prepare_hls_vod(inline_request(&server, playlist)).expect("fMP4 fallback prepare");
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("fMP4 topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert_eq!(request_count(&server, "/init.mp4"), 2);
    assert_eq!(request_count(&server, "/segment.m4s"), 1);
    assert!(
        first_attempt_body_bytes.load(Ordering::Acquire) < padded_initialization.len(),
        "TS-first NoMatch должен оборвать bounded prefix вместо полного fMP4 body"
    );
}

#[test]
fn restore_beyond_duration_fails_before_any_media_request() {
    let server = TestServer::start(|_, request| {
        panic!(
            "out-of-range restore must not request media: {}",
            request.request_line
        )
    });
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let error = prepare_hls_vod_receipted_at_start(
        inline_request(&server, playlist),
        async_seek_limits(),
        HlsVodStartIntent::Restore(MediaTime::from_secs(11)),
    )
    .expect_err("out-of-range restore");
    assert!(matches!(
        error,
        web_media_hls::HlsVodOpenError::InitialRestoreOutsideVod
    ));
    assert!(server.requests().is_empty());
}

#[test]
fn stale_checkpoint_falls_back_on_same_manifest_plan_and_one_beginning_segment() {
    let beginning_segment = Arc::new(muxed_ts_segment_with_early_landing(0));
    let manifest = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nbeginning.ts\n#EXT-X-ENDLIST\n";
    let server = TestServer::start({
        let beginning_segment = Arc::clone(&beginning_segment);
        move |_, request| {
            if request
                .request_line
                .starts_with("GET /stale-checkpoint.m3u8 ")
            {
                response("200 OK", &[], manifest)
            } else if request.request_line.starts_with("GET /beginning.ts ") {
                response("200 OK", &[], beginning_segment.as_slice())
            } else {
                panic!(
                    "unexpected stale-checkpoint fallback request: {}",
                    request.request_line
                );
            }
        }
    });

    let opened = prepare_hls_vod_receipted_at_start(
        fetched_request(&server, "/stale-checkpoint.m3u8"),
        async_seek_limits(),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_secs(355)),
    )
    .expect("stale checkpoint must reopen the parsed finite VOD from beginning");
    assert_eq!(
        opened.start_disposition(),
        HlsVodStartDisposition::RestoreRejectedToBeginning {
            target_position: MediaTime::from_secs(355),
            reason: HlsVodRestoreFallbackReason::CheckpointOutsideVod,
        }
    );
    assert!(matches!(
        opened.initial_position_proof(),
        HlsInitialPositionProofCapability::NotRequested
    ));
    let seek_handle = opened
        .async_seek_handle()
        .expect("receipted runtime must retain its public worker handle");
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()).expect("fallback beginning topology"),
        DemuxReadEvent::TracksChanged(_)
    ));
    let DemuxReadEvent::Packet(first_packet) =
        next_ready_event(demuxer.as_mut()).expect("fallback beginning packet")
    else {
        panic!("fallback must continue with the already opened beginning resource");
    };
    assert!(first_packet.pts < Duration::from_secs(10));
    assert_eq!(
        seek_handle.poll_receipt(),
        None,
        "manifest fallback не должен фабриковать worker seek receipt"
    );
    assert_eq!(request_count(&server, "/stale-checkpoint.m3u8"), 1);
    assert_eq!(request_count(&server, "/beginning.ts"), 1);
}
