//! End-to-end S32B evidence: prepare -> deferred worker -> concrete demux events.

#[path = "discontinuity/mod.rs"]
mod discontinuity;
mod support;

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::Encryptor;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
use demux_api::{
    DemuxOpenError, DemuxProbeRejection, ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, audio_fmp4, demux_registry, muxed_fmp4, muxed_ts,
    open_policy, range_response, response, ts_map_and_media, video_ts,
};
use web_media_hls::{
    ExtractorAesOverride, HlsAudioLayoutIntent, HlsAudioRenditionEvidence,
    HlsComponentContainerIntent, HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenError,
    HlsVodOpenRequest, SecretInlineMediaPlaylist, prepare_hls_vod, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn request(
    server: &TestServer,
    manifest_path: &str,
    selection: HlsVariantSelectionIntent,
    main_container: HlsRequiredContainer,
    alternate_audio: Option<HlsRequiredContainer>,
) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
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
        selection,
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(main_container),
            alternate_audio: alternate_audio.map(HlsContainerEvidence::Exact),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn inline_request(
    server: &TestServer,
    playlist: &str,
    container: HlsRequiredContainer,
) -> HlsVodOpenRequest {
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
            main: HlsContainerEvidence::Exact(container),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn next_ready_event(demuxer: &mut dyn Demuxer) -> Result<DemuxReadEvent, anyhow::Error> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let call_started = Instant::now();
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(
                    call_started.elapsed() < Duration::from_millis(50),
                    "player-owner poll must remain nonblocking"
                );
                assert!(Instant::now() < deadline, "deferred HLS worker timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return Ok(event),
        }
    }
}

fn assert_muxed_tracks(event: DemuxReadEvent) {
    let DemuxReadEvent::TracksChanged(update) = event else {
        panic!("initial TracksChanged expected");
    };
    assert_eq!(
        update
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .count(),
        1
    );
    assert_eq!(
        update
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .count(),
        1
    );
}

fn collect_until_eos(demuxer: &mut dyn Demuxer) -> Result<Vec<DemuxReadEvent>, anyhow::Error> {
    let mut events = Vec::new();
    for _ in 0..256 {
        let event = next_ready_event(demuxer)?;
        let ended = matches!(event, DemuxReadEvent::EndOfStream);
        events.push(event);
        if ended {
            return Ok(events);
        }
    }
    panic!("finite HLS fixture exceeded event bound");
}

fn encrypt_pkcs7(plaintext: &[u8], key: [u8; 16], iv: [u8; 16]) -> Vec<u8> {
    let mut buffer = vec![0_u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let encrypted_length = Encryptor::<Aes128>::new((&key).into(), (&iv).into())
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .expect("encrypt HLS fixture")
        .len();
    buffer.truncate(encrypted_length);
    buffer
}

fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

#[test]
fn muxed_ts_is_deferred_and_inline_manifest_causes_zero_manifest_fetch() {
    let segment = Arc::new(muxed_ts(90_000));
    let server_segment = Arc::clone(&segment);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/segment.ts") {
            std::thread::sleep(Duration::from_millis(100));
            response("200 OK", &[], &server_segment)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:1\n\
                    #EXTINF:1,\n\
                    segment.ts\n\
                    #EXT-X-ENDLIST\n";
    let opened = prepare_hls_vod(inline_request(
        &server,
        playlist,
        HlsRequiredContainer::TransportStream,
    ))
    .expect("prepare inline TS");
    let mut demuxer = opened.into_demuxer();
    let first_poll_started = Instant::now();
    assert!(matches!(
        demuxer.next_event().expect("first nonblocking poll"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    assert!(first_poll_started.elapsed() < Duration::from_millis(50));
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("TS tracks"));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].request_line.contains("/segment.ts"));
}

#[test]
fn muxed_fmp4_map_opens_through_injected_symphonia_factory() {
    let (initialization, first_media, _) = muxed_fmp4();
    let initialization = Arc::new(initialization);
    let first_media = Arc::new(first_media);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/media.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-MAP:URI=\"init.mp4\"\n\
                  #EXTINF:1,\nsegment.m4s\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &initialization)
        } else if request.request_line.contains("/segment.m4s") {
            response("200 OK", &[], &first_media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let opened = prepare_hls_vod(request(
        &server,
        "/media.m3u8",
        muxed_selection(),
        HlsRequiredContainer::FragmentedMp4,
        None,
    ))
    .expect("prepare muxed fMP4");
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("fMP4 tracks"));
}

#[test]
fn separate_ts_video_and_fmp4_audio_compose_before_initial_tracks() {
    let video = Arc::new(video_ts(90_000));
    let (audio_initialization, audio_media) = audio_fmp4();
    let audio_initialization = Arc::new(audio_initialization);
    let audio_media = Arc::new(audio_media);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",\
                  DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n\
                  #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",\
                  LANGUAGE=\"en\",URI=\"subtitle.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\",\
                  SUBTITLES=\"subs\"\nvideo.m3u8\n",
            )
        } else if request.request_line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nvideo.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/audio.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-MAP:URI=\"audio-init.mp4\"\n\
                  #EXTINF:1,\naudio.m4s\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/video.ts") {
            response("200 OK", &[], &video)
        } else if request.request_line.contains("/audio-init.mp4") {
            response("200 OK", &[], &audio_initialization)
        } else if request.request_line.contains("/audio.m4s") {
            response("200 OK", &[], &audio_media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let selection = HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
            name: Some("English".into()),
            ..HlsAudioRenditionEvidence::default()
        }),
        main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
    };
    let mut open_request = request(
        &server,
        "/master.m3u8",
        selection,
        HlsRequiredContainer::TransportStream,
        Some(HlsRequiredContainer::FragmentedMp4),
    );
    open_request.containers.alternate_audio = Some(HlsContainerEvidence::ContentProbe);
    let opened = prepare_hls_vod(open_request).expect("prepare separate TS/fMP4");
    assert_eq!(opened.subtitle_renditions().len(), 1);
    assert_eq!(opened.subtitle_renditions()[0].group_id(), "subs");
    assert_eq!(opened.subtitle_renditions()[0].characteristics(), None);
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("composite tracks"));
    assert!(
        server
            .requests()
            .iter()
            .all(|request| !request.request_line.contains("subtitle.m3u8"))
    );
}

#[test]
fn content_probe_unknown_audio_bytes_fail_during_prepare_with_typed_open_error() {
    let video = Arc::new(video_ts(90_000));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",\
                  DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\nvideo.m3u8\n",
            )
        } else if request.request_line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nvideo.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/audio.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-MAP:URI=\"broken-init.mp4\"\n\
                  #EXTINF:1,\nbroken.m4s\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/video.ts") {
            response("200 OK", &[], &video)
        } else if request.request_line.contains("/broken-init.mp4")
            || request.request_line.contains("/broken.m4s")
        {
            response("200 OK", &[], b"not-an-iso-bmff-container")
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let selection = HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
            name: Some("English".into()),
            ..HlsAudioRenditionEvidence::default()
        }),
        main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
    };
    let mut open_request = request(
        &server,
        "/master.m3u8",
        selection,
        HlsRequiredContainer::TransportStream,
        Some(HlsRequiredContainer::FragmentedMp4),
    );
    open_request.containers.alternate_audio = Some(HlsContainerEvidence::ContentProbe);
    let error = prepare_hls_vod(open_request).expect_err("content probe must reject unknown bytes");
    assert!(matches!(
        &error,
        HlsVodOpenError::AudioContainerProbeOpen(_)
    ));
    let diagnostic = format!("{error:#}");
    assert!(!diagnostic.contains("broken-init"));
    assert!(!diagnostic.contains("not-an-iso"));
}

#[test]
fn cancellation_during_audio_content_probe_remains_downcastable() {
    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let server_gate = Arc::clone(&gate);
    let audio_initialization = Arc::new(audio_fmp4().0);
    let audio_media = Arc::new(audio_fmp4().1);
    let video = Arc::new(video_ts(90_000));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",URI=\"audio.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\nvideo.m3u8\n",
            )
        } else if request.request_line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nvideo.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/audio.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-MAP:URI=\"audio-init.mp4\"\n\
                  #EXTINF:1,\naudio.m4s\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/audio-init.mp4") {
            let (lock, wake) = &*server_gate;
            let mut state = lock.lock().expect("probe gate");
            state.0 = true;
            wake.notify_all();
            while !state.1 {
                state = wake.wait(state).expect("probe gate wait");
            }
            response("200 OK", &[], &audio_initialization)
        } else if request.request_line.contains("/audio.m4s") {
            response("200 OK", &[], &audio_media)
        } else if request.request_line.contains("/video.ts") {
            response("200 OK", &[], &video)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let selection = HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Separate(HlsAudioRenditionEvidence {
            name: Some("English".into()),
            ..HlsAudioRenditionEvidence::default()
        }),
        main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
    };
    let mut open_request = request(
        &server,
        "/master.m3u8",
        selection,
        HlsRequiredContainer::TransportStream,
        Some(HlsRequiredContainer::FragmentedMp4),
    );
    open_request.containers.alternate_audio = Some(HlsContainerEvidence::ContentProbe);
    let cancellation = open_request.http.cancellation().clone();
    let worker = std::thread::spawn(move || prepare_hls_vod(open_request));
    {
        let (lock, wake) = &*gate;
        let mut state = lock.lock().expect("probe gate");
        while !state.0 {
            state = wake.wait(state).expect("probe gate wait");
        }
        cancellation.cancel();
        state.1 = true;
        wake.notify_all();
    }
    let error = worker
        .join()
        .expect("probe worker")
        .expect_err("cancelled probe must fail");
    assert!(matches!(
        &error,
        HlsVodOpenError::AudioContainerProbeOpen(DemuxOpenError::ProbeRejected(
            DemuxProbeRejection::Cancelled
        ))
    ));
}

#[test]
fn ts_map_with_explicit_and_implicit_ranges_is_content_proven_as_ts() {
    let (initialization, media) = ts_map_and_media(90_000);
    let initialization_length = initialization.len();
    let media_length = media.len();
    let mut shared = initialization;
    shared.extend_from_slice(&media);
    let shared = Arc::new(shared);
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
         #EXT-X-MAP:URI=\"shared.ts\",BYTERANGE=\"{initialization_length}@0\"\n\
         #EXTINF:1,\n#EXT-X-BYTERANGE:{media_length}\nshared.ts\n#EXT-X-ENDLIST\n"
    );
    let playlist = Arc::new(playlist.into_bytes());
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/ranges.m3u8") {
            response("200 OK", &[], &playlist)
        } else if request.request_line.contains("/shared.ts") {
            range_response(request, &shared)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let opened = prepare_hls_vod(request(
        &server,
        "/ranges.m3u8",
        muxed_selection(),
        HlsRequiredContainer::TransportStream,
        None,
    ))
    .expect("prepare TS MAP ranges");
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("TS MAP tracks"));
    let range_requests = server
        .requests()
        .into_iter()
        .filter(|request| request.request_line.contains("/shared.ts"))
        .collect::<Vec<_>>();
    assert_eq!(range_requests.len(), 2);
    assert!(range_requests.iter().all(|request| {
        request
            .headers
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("range: bytes="))
    }));
}

#[test]
fn external_aes_keys_merge_queries_cache_rotate_and_reset() {
    let first_key = [0x11_u8; 16];
    let second_key = [0x22_u8; 16];
    let explicit_iv = [0x33_u8; 16];
    let encrypted_a = Arc::new(encrypt_pkcs7(&muxed_ts(90_000), first_key, sequence_iv(7)));
    let encrypted_b = Arc::new(encrypt_pkcs7(&muxed_ts(180_000), first_key, sequence_iv(8)));
    let encrypted_c = Arc::new(encrypt_pkcs7(&muxed_ts(270_000), second_key, explicit_iv));
    let clear = Arc::new(muxed_ts(360_000));
    let playlist = Arc::new(
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:7\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"key-a?token=old\"\n\
             #EXTINF:1,\na.ts?token=old\n\
             #EXTINF:1,\nb.ts?token=old\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"key-a?token=old\",IV=0x{}\n\
             #EXTINF:1,\nc.ts?token=old\n\
             #EXT-X-KEY:METHOD=NONE\n\
             #EXTINF:1,\nclear.ts?token=old\n#EXT-X-ENDLIST\n",
            explicit_iv
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
        .into_bytes(),
    );
    let key_requests = Arc::new(AtomicUsize::new(0));
    let server_key_requests = Arc::clone(&key_requests);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/encrypted.m3u8") {
            response("200 OK", &[], &playlist)
        } else if request.request_line.contains("/key-a") {
            let key_index = server_key_requests.fetch_add(1, Ordering::AcqRel);
            if key_index == 0 {
                response("200 OK", &[], &first_key)
            } else {
                response("200 OK", &[], &second_key)
            }
        } else if request.request_line.contains("/a.ts") {
            response("200 OK", &[], &encrypted_a)
        } else if request.request_line.contains("/b.ts") {
            response("200 OK", &[], &encrypted_b)
        } else if request.request_line.contains("/c.ts") {
            response("200 OK", &[], &encrypted_c)
        } else if request.request_line.contains("/clear.ts") {
            response("200 OK", &[], &clear)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let mut open_request = request(
        &server,
        "/encrypted.m3u8",
        muxed_selection(),
        HlsRequiredContainer::TransportStream,
        None,
    );
    let target = server.target("/encrypted.m3u8");
    open_request.http = adaptive_context(
        &target,
        CancellationToken::new(),
        SourceGeneration::new(1),
        TestQueries {
            segment: Some("token=merged&segment=1"),
            key: Some("token=merged&segment=1"),
        },
    );
    let opened = prepare_hls_vod(open_request).expect("prepare encrypted TS");
    let mut demuxer = opened.into_demuxer();
    let events = collect_until_eos(&mut *demuxer).expect("decrypt full rotation playlist");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::Packet(_)))
    );
    let requests = server.requests();
    assert_eq!(key_requests.load(Ordering::Acquire), 2);
    assert!(
        requests
            .iter()
            .filter(|request| {
                request.request_line.contains(".ts") || request.request_line.contains("/key-")
            })
            .all(|request| {
                request.request_line.contains("token=merged")
                    && request.request_line.contains("segment=1")
                    && !request.request_line.contains("token=old")
            })
    );
}

#[test]
fn inline_aes_key_decrypts_without_key_request_and_exact_iv() {
    let key = [0x44_u8; 16];
    let iv = [0x55_u8; 16];
    let encrypted = Arc::new(encrypt_pkcs7(&muxed_ts(90_000), key, iv));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/inline-aes.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-KEY:METHOD=AES-128,URI=\"must-not-fetch\"\n\
                  #EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/segment.ts") {
            response("200 OK", &[], &encrypted)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let mut open_request = request(
        &server,
        "/inline-aes.m3u8",
        muxed_selection(),
        HlsRequiredContainer::TransportStream,
        None,
    );
    let key_hex = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let iv_hex = iv
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    open_request.overrides = HlsRequestOverrides::new(Some(
        ExtractorAesOverride::new(None, Some(&key_hex), Some(&iv_hex))
            .expect("inline AES override"),
    ));
    let opened = prepare_hls_vod(open_request).expect("prepare inline AES");
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("inline AES tracks"));
    assert!(
        server
            .requests()
            .iter()
            .all(|request| !request.request_line.contains("must-not-fetch"))
    );
}

#[test]
fn exact_aes_uri_bypasses_scoped_key_query_in_runtime() {
    let key = [0x48_u8; 16];
    let iv = [0x59_u8; 16];
    let encrypted = Arc::new(encrypt_pkcs7(&muxed_ts(90_000), key, iv));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/replacement.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                  #EXT-X-KEY:METHOD=AES-128,URI=\"manifest-key\"\n\
                  #EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/replacement-key") {
            response("200 OK", &[], &key)
        } else if request.request_line.contains("/segment.ts") {
            response("200 OK", &[], &encrypted)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let mut open_request = request(
        &server,
        "/replacement.m3u8",
        muxed_selection(),
        HlsRequiredContainer::TransportStream,
        None,
    );
    let target = server.target("/replacement.m3u8");
    open_request.http = adaptive_context(
        &target,
        CancellationToken::new(),
        SourceGeneration::new(1),
        TestQueries {
            segment: None,
            key: Some("key-query=must-not-apply"),
        },
    );
    open_request.overrides = HlsRequestOverrides::new(Some(
        ExtractorAesOverride::new(
            Some("replacement-key?exact=1"),
            None,
            Some("59595959595959595959595959595959"),
        )
        .expect("replacement AES override"),
    ));
    let opened = prepare_hls_vod(open_request).expect("prepare replacement URI");
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("replacement URI tracks"));
    let key_request = server
        .requests()
        .into_iter()
        .find(|request| request.request_line.contains("/replacement-key"))
        .expect("replacement key request");
    assert!(key_request.request_line.contains("exact=1"));
    assert!(!key_request.request_line.contains("must-not-apply"));
}

#[test]
fn encrypted_map_and_segments_share_key_only_inside_one_epoch() {
    let key = [0x5a_u8; 16];
    let iv = [0x6b_u8; 16];
    let (initialization, first_media, _) = muxed_fmp4();
    let encrypted_initialization = Arc::new(encrypt_pkcs7(&initialization, key, iv));
    let encrypted_media = Arc::new(encrypt_pkcs7(&first_media, key, iv));
    let key_requests = Arc::new(AtomicUsize::new(0));
    let server_key_requests = Arc::clone(&key_requests);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/key") {
            server_key_requests.fetch_add(1, Ordering::AcqRel);
            response("200 OK", &[], &key)
        } else if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &encrypted_initialization)
        } else if request.request_line.contains(".m4s") {
            response("200 OK", &[], &encrypted_media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"key\",\
                    IV=0x6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:1,\nfirst.m4s\n#EXT-X-DISCONTINUITY\n\
                    #EXTINF:1,\nsecond.m4s\n#EXT-X-ENDLIST\n";
    let opened = prepare_hls_vod(inline_request(
        &server,
        playlist,
        HlsRequiredContainer::FragmentedMp4,
    ))
    .expect("prepare encrypted MAP epochs");
    let mut demuxer = opened.into_demuxer();
    collect_until_eos(&mut *demuxer).expect("encrypted MAP epochs");
    assert_eq!(
        key_requests.load(Ordering::Acquire),
        2,
        "MAP+segment reuse inside each epoch, but discontinuity epoch refetches"
    );
}

#[test]
fn invalid_key_and_padding_fail_before_initial_tracks_changed() {
    for invalid_key in [true, false] {
        let key = [0x66_u8; 16];
        let iv = [0x77_u8; 16];
        let mut encrypted = encrypt_pkcs7(&muxed_ts(90_000), key, iv);
        if !invalid_key {
            let last = encrypted.last_mut().expect("ciphertext byte");
            *last ^= 0xff;
        }
        let encrypted = Arc::new(encrypted);
        let server = TestServer::start(move |_, request| {
            if request.request_line.contains("/invalid.m3u8") {
                response(
                    "200 OK",
                    &[],
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                      #EXT-X-KEY:METHOD=AES-128,URI=\"key\",\
                      IV=0x77777777777777777777777777777777\n\
                      #EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
                )
            } else if request.request_line.contains("/key") {
                if invalid_key {
                    response("200 OK", &[], &[0x66; 15])
                } else {
                    response("200 OK", &[], &key)
                }
            } else if request.request_line.contains("/segment.ts") {
                response("200 OK", &[], &encrypted)
            } else {
                response("404 Not Found", &[], b"")
            }
        });
        let opened = prepare_hls_vod(request(
            &server,
            "/invalid.m3u8",
            muxed_selection(),
            HlsRequiredContainer::TransportStream,
            None,
        ))
        .expect("manifest/profile prepare succeeds");
        let mut demuxer = opened.into_demuxer();
        let error = next_ready_event(&mut *demuxer).expect_err("preflight failure");
        let diagnostic = format!("{error:#}");
        assert!(!diagnostic.contains("segment.ts"));
        assert!(!diagnostic.contains("777777"));
    }
}

#[test]
fn stale_cancel_and_missing_container_fail_without_publication_or_network() {
    let server = TestServer::start(|_, _| response("500 Internal Server Error", &[], b""));
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nnever.ts\n#EXT-X-ENDLIST\n";

    let mut stale = inline_request(&server, playlist, HlsRequiredContainer::TransportStream);
    stale.generation = SourceGeneration::new(2);
    assert!(matches!(
        prepare_hls_vod(stale),
        Err(HlsVodOpenError::Transport(
            web_media_adaptive::AdaptiveTransportError::StaleGeneration { .. }
        ))
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut cancelled = inline_request(&server, playlist, HlsRequiredContainer::TransportStream);
    let target = server.target("/authoritative-inline.m3u8");
    cancelled.http = adaptive_context(
        &target,
        cancellation,
        SourceGeneration::new(1),
        TestQueries::default(),
    );
    assert!(matches!(
        prepare_hls_vod(cancelled),
        Err(HlsVodOpenError::Transport(
            web_media_adaptive::AdaptiveTransportError::Cancelled
        ))
    ));

    let mut missing = inline_request(&server, playlist, HlsRequiredContainer::TransportStream);
    missing.containers.main = HlsContainerEvidence::Missing;
    assert!(matches!(
        prepare_hls_vod(missing),
        Err(HlsVodOpenError::MissingMainContainerEvidence)
    ));
    let mut ambiguous = inline_request(&server, playlist, HlsRequiredContainer::TransportStream);
    ambiguous.containers.main = HlsContainerEvidence::Ambiguous;
    assert!(matches!(
        prepare_hls_vod(ambiguous),
        Err(HlsVodOpenError::AmbiguousMainContainerEvidence)
    ));
    assert!(server.requests().is_empty());
}

#[test]
fn vod_seek_uses_proven_raps_across_discontinuity_and_fences_rapid_supersede() {
    let first = Arc::new(muxed_ts(0));
    let second = Arc::new(muxed_ts(0));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/first.ts") {
            response("200 OK", &[], &first)
        } else if request.request_line.contains("/second.ts") {
            response("200 OK", &[], &second)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:1\n\
                    #EXTINF:1,\nfirst.ts\n\
                    #EXT-X-DISCONTINUITY\n\
                    #EXTINF:1,\nsecond.ts\n\
                    #EXT-X-ENDLIST\n";
    let opened = prepare_hls_vod_receipted(
        inline_request(&server, playlist, HlsRequiredContainer::TransportStream),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(4).expect("seek receipt bound")),
    )
    .expect("prepare seekable discontinuous VOD");
    let seek_handle = opened.async_seek_handle().expect("receipted seek handle");
    let mut demuxer = opened.into_demuxer();
    assert_muxed_tracks(next_ready_event(&mut *demuxer).expect("initial tracks"));
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let event = next_ready_event(&mut *demuxer).expect("build VOD seek index");
        if matches!(event, DemuxReadEvent::Packet(packet) if packet.pts == Duration::from_secs(1)) {
            break;
        }
        assert!(Instant::now() < deadline, "second epoch was not indexed");
    }

    let forward_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            forward_fence,
            DemuxSeekRequest::decode_point_before(Duration::from_millis(1_500)),
        )
        .expect("enqueue forward worker seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let forward_receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "forward seek receipt timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    let ProgressiveAsyncSeekOutcome::Succeeded(forward) = forward_receipt.outcome else {
        panic!("forward seek must succeed: {forward_receipt:?}");
    };
    assert_eq!(
        forward.actual_position.as_duration(),
        Duration::from_secs(1)
    );
    let backward_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(2),
    };
    seek_handle
        .enqueue(
            backward_fence,
            DemuxSeekRequest::decode_point_before(Duration::from_millis(500)),
        )
        .expect("enqueue backward worker seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let backward_receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "backward seek receipt timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    let ProgressiveAsyncSeekOutcome::Succeeded(backward) = backward_receipt.outcome else {
        panic!("backward seek must succeed: {backward_receipt:?}");
    };
    assert_eq!(backward.actual_position.as_duration(), Duration::ZERO);

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let event = next_ready_event(&mut *demuxer).expect("post-seek event");
        if let DemuxReadEvent::Packet(packet) = event {
            assert_eq!(packet.kind, TrackKind::Video);
            assert_eq!(packet.pts, Duration::ZERO);
            break;
        }
        assert!(Instant::now() < deadline, "rapid seek did not land");
    }
}
