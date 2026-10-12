//! Регрессия: HLS VOD с VP9 в fragmented MP4 (формат YouTube, itag 616 и соседние)
//! открывается и отдаёт keyframe-пакет.
//!
//! Корень: sample entry `vp09` не давал трекам codec id, поэтому у пакетов не определялся
//! keyframe, и initial seek anchor VOD-а не находился («HLS VOD завершился до доказанного
//! initial seek anchor»).

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

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use demux_api::ProgressiveAsyncSeekLimits;
use media_core::{DemuxReadEvent, PacketKeyframe, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, decode_base64, demux_registry, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Video-only VP9 fMP4 (`ffmpeg -c:v libvpx-vp9 -g 2 -movflags frag_keyframe+empty_moov`),
/// 4 кадра, keyframe-ы на кадрах 0 и 2.
const VP9_FMP4_BASE64: &str = include_str!("fixtures/vp9-video-fmp4.base64");

/// Делит fragmented MP4 на init (`ftyp+moov`) и media (всё от первого `moof`).
fn split_init_and_media(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut position = 0_usize;
    while position + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[position..position + 4].try_into().unwrap()) as usize;
        if &bytes[position + 4..position + 8] == b"moof" {
            return (bytes[..position].to_vec(), bytes[position..].to_vec());
        }
        assert!(size >= 8, "Broken top-level box in fixture");
        position += size;
    }
    panic!("fixture has no moof");
}

#[test]
fn vp9_video_only_fmp4_vod_opens_and_publishes_keyframe_packet() {
    let (initialization, media) = split_init_and_media(&decode_base64(VP9_FMP4_BASE64));
    let initialization = Arc::new(initialization);
    let media = Arc::new(media);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &initialization)
        } else if request.request_line.contains("/segment.m4s") {
            response("200 OK", &[], &media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let generation = SourceGeneration::new(1);
    let selected_url = server.target("/media.m3u8");
    let playlist = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:1\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:0.4,\nsegment.m4s\n#EXT-X-ENDLIST\n";
    let request = HlsVodOpenRequest {
        http: adaptive_context(
            &selected_url,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::InlineMedia {
            selected_url,
            playlist: web_media_hls::SecretInlineMediaPlaylist::new(playlist),
        },
        selection: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::VideoOnly,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::FragmentedMp4),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    };

    let opened = prepare_hls_vod_receipted(
        request,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("VP9 fMP4 VOD should be prepared");
    let mut demuxer = opened.into_demuxer();

    let deadline = Instant::now() + TEST_TIMEOUT;
    let first_packet = loop {
        match demuxer.next_event().expect("VP9 fMP4 Events Read") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "The HLS worker is not ready.");
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::EndOfStream => panic!("The stream ended before the first packet."),
            _ => {}
        }
    };
    assert_eq!(first_packet.kind, TrackKind::Video);
    assert_eq!(first_packet.keyframe, PacketKeyframe::Keyframe);
}
