//! A13-3: HLS без видео (audio-only, интернет-радио) открывается и играет, VOD и live.
//!
//! Тесты идут настоящим путём native-ingress: admission top media playlist -> intent и
//! container evidence из admission -> `prepare_hls_vod_receipted`/`prepare_hls_live` -> пакеты.
//! Раньше форма top media playlist была «видео обязательно», и звук без видео отвергался
//! ошибкой «HLS main track shape mismatch», а live не получал бы decode anchor из аудио.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)] // Shared fixture экспортирует больше builders, чем нужно этому focused binary.
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use demux_api::{ProgressiveAsyncSeekLimits, ProgressiveSeekFence, ProgressiveSeekRequestId};
use hls_playlist_core::HlsParserLimits;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, DynamicMediaTimelineEpoch,
    DynamicMediaTimelinePortGeneration, Packet, TrackKind,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_audio_ts_segment, muxed_ts,
    open_policy, response,
};
use web_media_core::{
    AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity, CodecFamily,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, ExactSelectionIdentity,
    ExtractionGeneration, NormalizedCodec, PreferredHeightPolicy, RawCodecIdentity,
    SemanticIdentity, SourceIdentity, VideoTrackDescriptor,
};
use web_media_hls::{
    HlsCatalogBuildPolicy, HlsCatalogCapabilityProofPort, HlsCatalogCapabilityRejection,
    HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest, HlsCatalogPresentation,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsManifestInput, HlsProviderDefaultAudioPolicy,
    HlsRequestOverrides, HlsVodOpenRequest, NativeHlsCatalogAdmission, NativeHlsSelectionPolicy,
    SecretInlineMediaPlaylist, admit_native_hls_catalog, discover_hls_catalog,
    prepare_hls_catalog_vod_receipted, prepare_hls_live, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);
const PTS_PER_SECOND: u64 = 90_000;
/// Секунд звука (по одному AAC-фрейму на секунду) в каждом VOD-сегменте.
const VOD_SEGMENT_SECONDS: u64 = 10;

fn policy() -> NativeHlsSelectionPolicy {
    NativeHlsSelectionPolicy::new(
        PreferredHeightPolicy::NoPreference,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy")
}

/// Admission ровно так, как его делает production: top media playlist без master.
fn admit(playlist: &str, target: &HttpRequestTarget) -> NativeHlsCatalogAdmission {
    admit_native_hls_catalog(
        playlist.as_bytes(),
        target,
        HlsParserLimits::default(),
        &policy(),
    )
    .expect("audio-only media playlist должен пройти native admission")
}

fn open_request(
    target: HttpRequestTarget,
    manifest: HlsManifestInput,
    admission: &NativeHlsCatalogAdmission,
) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest,
        selection: admission.runtime_intent(),
        overrides: HlsRequestOverrides::new(None),
        containers: admission.container_intent(),
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn inline_vod_request(
    server: &TestServer,
    playlist: &'static str,
    admission: &NativeHlsCatalogAdmission,
) -> HlsVodOpenRequest {
    let target = server.target("/media.m3u8");
    open_request(
        target.clone(),
        HlsManifestInput::InlineMedia {
            selected_url: target,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        admission,
    )
}

fn next_ready(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("HLS runtime читается") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS worker не стал готов");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Читает события до первого пакета; попутно собирает виды опубликованных дорожек.
fn first_packet_and_track_kinds(demuxer: &mut dyn Demuxer) -> (Packet, Vec<TrackKind>) {
    let mut track_kinds = Vec::new();
    loop {
        match next_ready(demuxer) {
            DemuxReadEvent::TracksChanged(update) => {
                track_kinds = update.tracks.iter().map(|track| track.kind).collect();
            }
            DemuxReadEvent::Packet(packet) => return (packet, track_kinds),
            DemuxReadEvent::EndOfStream => panic!("поток закончился до первого пакета"),
            _ => {}
        }
    }
}

fn vod_ts_playlist() -> &'static str {
    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-PLAYLIST-TYPE:VOD\n\
     #EXTINF:10,\nseg0.ts\n#EXTINF:10,\nseg1.ts\n#EXTINF:10,\nseg2.ts\n#EXT-X-ENDLIST\n"
}

/// Audio-only ts VOD: форма «только звук» принята, длительность - сумма EXTINF, видео нет,
/// первый пакет - звук, а перемотка садится на аудио-пакет нужного сегмента.
#[test]
fn audio_only_ts_vod_opens_reaches_audio_packets_and_seeks() {
    let server = TestServer::start(move |_, request| {
        for index in 0..3_u64 {
            if request.request_line.contains(&format!("/seg{index}.ts")) {
                let start = index * VOD_SEGMENT_SECONDS * PTS_PER_SECOND;
                return response(
                    "200 OK",
                    &[],
                    &long_audio_ts_segment(start, VOD_SEGMENT_SECONDS),
                );
            }
        }
        response("404 Not Found", &[], b"")
    });
    let admission = admit(vod_ts_playlist(), &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, vod_ts_playlist(), &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("audio-only ts VOD должен открыться");
    assert_eq!(opened.duration(), Duration::from_secs(30));
    let seek_handle = opened.async_seek_handle().expect("receipted seek handle");
    let mut demuxer = opened.into_demuxer();

    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Audio], "видео быть не должно");
    assert_eq!(packet.kind, TrackKind::Audio);

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(15)),
        )
        .expect("enqueue seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "seek receipt не пришёл");
        std::thread::sleep(Duration::from_millis(2));
    };
    let demux_api::ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("seek audio-only VOD должен удаться: {receipt:?}");
    };
    let landed = result.actual_position.as_duration();
    assert!(
        landed >= Duration::from_secs(10) && landed <= Duration::from_secs(15),
        "seek должен сесть во второй сегмент не позже цели, получено {landed:?}"
    );
    loop {
        if let DemuxReadEvent::Packet(packet) = next_ready(demuxer.as_mut()) {
            assert_eq!(packet.kind, TrackKind::Audio);
            assert!(
                packet.pts >= Duration::from_secs(10),
                "после seek первый пакет не раньше начала второго сегмента: {:?}",
                packet.pts
            );
            break;
        }
    }
}

/// Audio-only fMP4 VOD (EXT-X-MAP): тот же путь, контейнер определяется по содержимому.
#[test]
fn audio_only_fmp4_vod_opens_and_reaches_audio_packet() {
    let (init, media) = support::audio_fmp4();
    let (init, media) = (Arc::new(init), Arc::new(media));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &init)
        } else if request.request_line.contains("/seg.m4s") {
            response("200 OK", &[], &media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\nseg.m4s\n\
        #EXT-X-ENDLIST\n";
    let admission = admit(PLAYLIST, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, PLAYLIST, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("audio-only fMP4 VOD должен открыться");
    let mut demuxer = opened.into_demuxer();
    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Audio]);
    assert_eq!(packet.kind, TrackKind::Audio);
}

struct NoRefresh;
impl HlsEndpointRefreshPort for NoRefresh {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::Cancelled)
    }
}

// Сегмент по 2 с: два AAC-пакета (на 0 с и 1 с) задают покрытие; у TS-пакетов нет duration,
// поэтому конец покрытия сегмента - PTS его последнего пакета.
const LIVE_FIRST_WINDOW: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
    #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n";
const LIVE_SLID_WINDOW: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
    #EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:2,\nb.ts\n#EXTINF:2,\nc.ts\n";

/// Audio-only ts live со скользящим окном: открывается, отдаёт аудио-пакеты, а DVR-диапазон
/// строится из аудио-якорей и сдвигается вперёд при обновлении плейлиста. Без вывода
/// random-access политики из фактической формы диапазона не было бы вовсе (якорь только видео).
#[test]
fn audio_only_live_ts_sliding_window_publishes_audio_anchored_dvr_and_advances() {
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let handler_manifest_requests = Arc::clone(&manifest_requests);
    let segments = [
        long_audio_ts_segment(PTS_PER_SECOND, 2),
        long_audio_ts_segment(3 * PTS_PER_SECOND, 2),
        long_audio_ts_segment(5 * PTS_PER_SECOND, 2),
    ];
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /live.m3u8 ") {
            let count = handler_manifest_requests.fetch_add(1, Ordering::SeqCst);
            let playlist = if count == 0 {
                LIVE_FIRST_WINDOW
            } else {
                LIVE_SLID_WINDOW
            };
            return response("200 OK", &[], playlist.as_bytes());
        }
        for (name, segment) in ["a", "b", "c"].iter().zip(&segments) {
            if request
                .request_line
                .starts_with(&format!("GET /{name}.ts "))
            {
                return response("200 OK", &[], segment);
            }
        }
        response("404 Not Found", &[], b"")
    });
    let target = server.target("/live.m3u8");
    let admission = admit(LIVE_FIRST_WINDOW, &target);
    let opened = prepare_hls_live(HlsLiveOpenRequest {
        common: open_request(
            target.clone(),
            HlsManifestInput::Fetch {
                selected_url: target,
            },
            &admission,
        ),
        endpoint_refresh: Arc::new(NoRefresh),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    })
    .expect("audio-only live должен открыться");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    let (packet, _) = first_packet_and_track_kinds(demuxer.as_mut());
    let track_kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|track| track.kind).collect();
    assert_eq!(track_kinds, vec![TrackKind::Audio]);
    assert_eq!(packet.kind, TrackKind::Audio);

    // Читаем, пока окно не сдвинулось: начало DVR-диапазона уходит вперёд после refresh.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let slid_range = loop {
        let observed = timeline_port.observe();
        if let Some(range) = observed.snapshot.state.seekable_range()
            && manifest_requests.load(Ordering::SeqCst) >= 2
            && range.start.as_duration() >= Duration::from_secs(2)
        {
            break range;
        }
        assert!(
            Instant::now() < deadline,
            "DVR-диапазон audio-only live не продвинулся: {:?}",
            observed.snapshot.state.seekable_range()
        );
        let _ = demuxer.next_event().expect("audio-only live читается");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(slid_range.end > slid_range.start);
}

/// Регрессия: muxed (видео+звук) через тот же admission по-прежнему отдаёт обе дорожки,
/// а звук внутри видео-потока не становится якорем (форма «видео» сохраняет видео-политику).
#[test]
fn muxed_ts_still_opens_with_both_tracks_through_the_same_admission() {
    let ts = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains(".ts") {
            response("200 OK", &[], &ts)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    const TS: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
    let admission = admit(TS, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, TS, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("muxed VOD должен открыться");
    let mut demuxer = opened.into_demuxer();
    let (_, mut kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    kinds.sort_by_key(|kind| matches!(kind, TrackKind::Audio));
    assert_eq!(kinds, vec![TrackKind::Video, TrackKind::Audio]);
}

/// Capability-порт радио: знает только звук AAC; видео в проверяемом child быть не должно.
struct AudioOnlyCapabilities;

impl HlsCatalogCapabilityProofPort for AudioOnlyCapabilities {
    fn prove_video(
        &mut self,
        _track: &media_core::TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection> {
        panic!("в audio-only child не должно быть видео-дорожки");
    }

    fn prove_audio(
        &mut self,
        track: &media_core::TrackInfo,
    ) -> Result<AudioTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Audio);
        Ok(AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("audio codec")),
            None,
            None,
            None,
            None,
        ))
    }
}

const RADIO_MASTER: &str = "#EXTM3U\n\
    #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\nlow.m3u8\n\
    #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nhigh.m3u8\n";

/// Master радио (варианты только с mp4a в CODECS, без RESOLUTION): admission выбирает вариант
/// с большим bandwidth, каталог доказывает audio-only, provider default - AudioOnly, а
/// воспроизведение доходит до аудио-пакетов именно выбранного (high) child-а.
#[test]
fn audio_only_master_picks_best_bandwidth_and_plays_through_catalog() {
    let high_requests = Arc::new(AtomicUsize::new(0));
    let high_seen = Arc::clone(&high_requests);
    let segment = long_audio_ts_segment(PTS_PER_SECOND, 2);
    let server = TestServer::start(move |_, request| {
        // Длительности различаются, чтобы по ним отличить, какой child реально открыт.
        let child = |name: &str, seconds: u32| {
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:{seconds}\n#EXTINF:{seconds},\n{name}.ts\n\
                 #EXT-X-ENDLIST\n"
            )
        };
        if request.request_line.starts_with("GET /master.m3u8 ") {
            response("200 OK", &[], RADIO_MASTER.as_bytes())
        } else if request.request_line.starts_with("GET /high.m3u8 ") {
            response("200 OK", &[], child("high", 2).as_bytes())
        } else if request.request_line.starts_with("GET /low.m3u8 ") {
            response("200 OK", &[], child("low", 4).as_bytes())
        } else if request.request_line.starts_with("GET /high.ts ") {
            high_seen.fetch_add(1, Ordering::SeqCst);
            response("200 OK", &[], &segment)
        } else if request.request_line.starts_with("GET /low.ts ") {
            response("200 OK", &[], &segment)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let target = server.target("/master.m3u8");
    let admission = admit(RADIO_MASTER, &target);
    assert_eq!(
        admission.runtime_intent().main_track_layout,
        web_media_hls::HlsMainTrackLayoutIntent::AudioOnly
    );
    assert_eq!(
        admission.current_master_variant_index(),
        Some(1),
        "радио выбирает вариант с большим bandwidth"
    );
    let open = open_request(
        target.clone(),
        HlsManifestInput::Fetch {
            selected_url: target,
        },
        &admission,
    );
    let source = SourceIdentity::new(93);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hls-radio").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-radio").expect("semantic identity");
    let outcome = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &open,
            catalog_identity: ComponentVariantCatalogIdentity::new(
                ExactSelectionIdentity::new(exact, semantic).expect("same source"),
                ComponentVariantCatalogGeneration::new(1),
            ),
            presentation: HlsCatalogPresentation::Vod,
            provider_default_variant_index: admission.current_master_variant_index(),
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
                provider_default_audio: HlsProviderDefaultAudioPolicy::RequireDeclared,
            },
        },
        &mut AudioOnlyCapabilities,
    )
    .expect("audio-only master должен пройти discovery");
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = outcome else {
        panic!("master discovery must install a catalog");
    };
    assert!(matches!(
        snapshot.provider_default_selection(),
        web_media_core::ComponentVariantSelection::AudioOnly { .. }
    ));
    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("provider default audio-only");
    let opened = prepare_hls_catalog_vod_receipted(
        open,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("audio-only master child должен открыться");
    assert_eq!(
        opened.duration(),
        Duration::from_secs(2),
        "открыт должен быть high (EXTINF 2 с), а не low (4 с)"
    );
    let mut demuxer = opened.into_demuxer();
    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Audio]);
    assert_eq!(packet.kind, TrackKind::Audio);
    assert!(high_requests.load(Ordering::SeqCst) >= 1, "high играет");
}
