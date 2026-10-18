//! Сквозной тест DASH с РАЗДЕЛЬНЫМИ видео и аудио и длинным GOP на настоящем открытии
//! `prepare_dash_vod` (loopback-сервер, fMP4, ffmpeg-фикстуры): первый keyframe видео
//! обязан дойти до потребителя первым пакетом.
//!
//! Регрессия: каждый native DASH component начинает поток с `[TracksChanged, Packet(anchor)]`.
//! Replay аудио стирал уже сохранённый композитом keyframe видео и (через наружный
//! `TracksChanged`) сбрасывал декодер; при GOP в несколько секунд поток навсегда
//! застревал в Buffering, потому что декодер выбрасывает delta-кадры до следующего keyframe.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic здесь работают
// как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
#[path = "../../web-media-hls/tests/support/mod.rs"]
mod support;

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashContainer, DashMediaKind, DashMpdLimits};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveDemuxBufferLimits,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, PacketKeyframe, TrackKind};
use source_core::CancellationToken;
use support::{ObservedRequest, TestQueries, TestServer, adaptive_context, demux_registry};
use web_media_dash::{
    DashManifestInput, DashPresentationSelection, DashRepresentationEvidence, DashVodHttpContext,
    DashVodInput, DashVodOpenPolicy, DashVodOpenRequest, prepare_dash_vod,
};
use web_media_transport_api::SourceGeneration;

/// Предел ожидания фонового worker-а demux.
const WAIT: Duration = Duration::from_secs(8);
/// Видео-фикстура: 25 кадров, ровно один keyframe (первый).
const VIDEO_FIXTURE: &[u8] = include_bytes!("fixtures/dash-long-gop/video-long-gop.mp4");
/// Аудио-фикстура: AAC-LC, один fMP4-фрагмент.
const AUDIO_FIXTURE: &[u8] = include_bytes!("fixtures/dash-long-gop/audio.mp4");
/// Сколько кадров лежит в видео-фикстуре (см. `generate.sh`).
const VIDEO_FIXTURE_FRAME_COUNT: usize = 25;

/// Раздельные video/audio Representation, по одному сегменту каждая (SegmentList).
const MANIFEST: &str = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
    mediaPresentationDuration="PT1S">
  <Period id="p0" duration="PT1S">
    <AdaptationSet id="v" contentType="video" mimeType="video/mp4" codecs="avc1.42c00a"
        width="64" height="64">
      <Representation id="video" bandwidth="200000" width="64" height="64">
        <SegmentList timescale="1" duration="1">
          <Initialization sourceURL="video-init.mp4"/>
          <SegmentURL media="video-0.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
    <AdaptationSet id="a" contentType="audio" mimeType="audio/mp4" codecs="mp4a.40.2">
      <Representation id="audio" bandwidth="64000" audioSamplingRate="48000">
        <SegmentList timescale="1" duration="1">
          <Initialization sourceURL="audio-init.mp4"/>
          <SegmentURL media="audio-0.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

// ----- политика и открытие --------------------------------------------------------------------

fn dash_policy() -> DashVodOpenPolicy {
    DashVodOpenPolicy {
        maximum_manifest_bytes: NonZeroUsize::new(64 * 1_024).unwrap(),
        maximum_fragment_bytes: NonZeroUsize::new(256 * 1_024).unwrap(),
        maximum_range_read_bytes: NonZeroUsize::new(16 * 1_024).unwrap(),
        maximum_cached_range_pages: NonZeroUsize::new(2).unwrap(),
        maximum_planned_segments: NonZeroUsize::new(64).unwrap(),
        maximum_parallel_catalog_probes: NonZeroUsize::new(4).unwrap(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(8 * 1_024).unwrap(),
            NonZeroUsize::new(4).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap(),
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(64).unwrap(),
            NonZeroUsize::new(512 * 1_024).unwrap(),
        ),
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(4).unwrap()),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(5)).unwrap(),
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(2),
            NonZeroUsize::new(256 * 1_024).unwrap(),
        )
        .unwrap(),
        maximum_seek_scan_events: NonZeroUsize::new(4_096).unwrap(),
        maximum_seek_scan_bytes: NonZeroUsize::new(8 * 1_024 * 1_024).unwrap(),
    }
}

fn manifest_input(target: source_core::HttpRequestTarget) -> DashManifestInput {
    DashManifestInput {
        target,
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1024)
            .maximum_depth(32)
            .maximum_tokens(4_096)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(2_048)
            .maximum_attribute_bytes(32 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(32 * 1024)
            .build()
            .unwrap(),
        mpd_limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 16,
            maximum_representations_per_adaptation_set: 16,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4 * 1024,
        },
    }
}

/// Режет fragmented MP4 на `(init, media)` по границе первого `moof`; хвост `mfra` отбрасывается.
fn split_init_and_media(fragmented: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut offset = 0;
    let mut first_moof = None;
    let mut media_end = fragmented.len();
    while offset + 8 <= fragmented.len() {
        let size = u32::from_be_bytes(fragmented[offset..offset + 4].try_into().unwrap());
        let kind = &fragmented[offset + 4..offset + 8];
        let size = usize::try_from(size).unwrap();
        assert!(size >= 8, "MP4 box size is sane");
        match kind {
            b"moof" => first_moof = first_moof.or(Some(offset)),
            b"mfra" => media_end = offset,
            _ => {}
        }
        offset += size;
    }
    let first_moof = first_moof.expect("fixture has a media fragment");
    (
        fragmented[..first_moof].to_vec(),
        fragmented[first_moof..media_end].to_vec(),
    )
}

/// Loopback-сервер: манифест и по одному сегменту видео/аудио, остальное - 404.
fn start_server() -> TestServer {
    let (video_init, video_media) = split_init_and_media(VIDEO_FIXTURE);
    let (audio_init, audio_media) = split_init_and_media(AUDIO_FIXTURE);
    let routes: Vec<(&'static str, Vec<u8>)> = vec![
        ("/manifest.mpd", MANIFEST.as_bytes().to_vec()),
        ("/video-init.mp4", video_init),
        ("/video-0.m4s", video_media),
        ("/audio-init.mp4", audio_init),
        ("/audio-0.m4s", audio_media),
    ];
    TestServer::start(move |_, request: &ObservedRequest| {
        let path = request
            .request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default();
        match routes.iter().find(|(route, _)| *route == path) {
            Some((_, body)) => support::range_response(request, body),
            None => support::response("404 Not Found", &[], b""),
        }
    })
}

/// Evidence одной Representation по её `id` в MPD.
fn evidence(kind: DashMediaKind, representation_id: &str) -> DashRepresentationEvidence {
    DashRepresentationEvidence {
        media_kind: kind,
        container: DashContainer::IsoBmff,
        representation_id: Some(representation_id.to_owned()),
        codecs: None,
        bandwidth: None,
        dimensions: None,
    }
}

/// Открывает раздельную пару video+audio production-путём `prepare_dash_vod`.
fn open_separate_av(server: &TestServer) -> Box<dyn Demuxer + Send> {
    let target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let opened = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(target)),
        selection: DashPresentationSelection::Separate {
            video: evidence(DashMediaKind::Video, "video"),
            audio: evidence(DashMediaKind::Audio, "audio"),
        },
        demux_registry: demux_registry(),
        policy: dash_policy(),
    })
    .expect("separate A/V DASH VOD prepares");
    Box::new(opened.into_demuxer())
}

/// Событие потока в компактной форме для проверки порядка.
#[derive(Debug, PartialEq)]
enum Observed {
    VideoPacket { keyframe: bool, pts: Duration },
    AudioPacket,
    TracksChanged,
}

/// Читает demuxer до EOS, ожидая фонового worker-а при `TemporarilyUnavailable`.
fn read_until_end(demuxer: &mut dyn Demuxer) -> Vec<Observed> {
    let deadline = Instant::now() + WAIT;
    let mut observed = Vec::new();
    loop {
        match demuxer.next_event().expect("DASH stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "DASH worker timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                observed.push(Observed::VideoPacket {
                    keyframe: packet.keyframe == PacketKeyframe::Keyframe,
                    pts: packet.pts,
                });
            }
            DemuxReadEvent::Packet(_) => observed.push(Observed::AudioPacket),
            DemuxReadEvent::TracksChanged(_) => observed.push(Observed::TracksChanged),
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => return observed,
        }
    }
}

/// Первый пакет раздельного A/V DASH - keyframe pts 0, и ни один video-пакет не потерян.
#[test]
fn separate_av_dash_with_long_gop_delivers_first_keyframe_first() {
    let server = start_server();
    let mut demuxer = open_separate_av(&server);

    let observed = read_until_end(demuxer.as_mut());

    // Без исправления первым приходил TracksChanged, а keyframe терялся.
    let first_packet = observed
        .iter()
        .find(|event| !matches!(event, Observed::TracksChanged))
        .expect("stream delivers packets");
    assert_eq!(
        first_packet,
        &Observed::VideoPacket {
            keyframe: true,
            pts: Duration::ZERO
        },
        "the only keyframe must be the first delivered packet: {observed:?}"
    );
    let video_packets: Vec<_> = observed
        .iter()
        .filter_map(|event| match event {
            Observed::VideoPacket { keyframe, .. } => Some(*keyframe),
            _ => None,
        })
        .collect();
    assert_eq!(video_packets.len(), VIDEO_FIXTURE_FRAME_COUNT);
    assert_eq!(video_packets.iter().filter(|key| **key).count(), 1);
    assert!(observed.contains(&Observed::AudioPacket));
    // Все TracksChanged стартового replay приходят строго до первого packet-а.
    let first_packet_index = observed
        .iter()
        .position(|event| !matches!(event, Observed::TracksChanged))
        .unwrap();
    assert!(
        observed[first_packet_index..]
            .iter()
            .all(|event| !matches!(event, Observed::TracksChanged)),
        "start replays must not interleave with media packets: {observed:?}"
    );
}
