//! 16d3 (решение 333): обычный плеер, HLS-эфир по ссылке. Пока «машина спала», окно
//! эфира ушло вперёд целиком; после разрыва кадр доходит до декодера и рендера, а звук —
//! до PCM, без роковой ошибки «live refresh continuity failed».

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use web_media_hls::HlsVodStartIntent;

use super::native_hls_vertical::{
    ControlledHlsServer, assert_decoder_render_audio_movement_after, decode_fixture,
    native_settings, prepare_native,
};
use crate::media_open::{NativeHlsUrl, SafeMediaLabel};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;
use media_source_open::native_startup::hls::PreparedNativeHlsLifecycle;

/// Конец старого окна на шкале плеера: два секундных сегмента.
const OLD_LIVE_EDGE: Duration = Duration::from_secs(2);

fn live_master() -> Vec<u8> {
    concat!(
        "#EXTM3U\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,",
        "CODECS=\"avc1.42c00a,mp4a.40.2\"\n",
        "live.m3u8\n",
    )
    .as_bytes()
    .to_vec()
}

/// Окно из двух секундных сегментов, начиная с media sequence `first`.
fn window(first: u64) -> Vec<u8> {
    format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n\
         #EXTINF:1,\nts-{first}.ts\n#EXTINF:1,\nts-{}.ts\n",
        first + 1
    )
    .into_bytes()
}

/// Первые три плейлиста — старое окно 0..1, дальше — окно «после сна» 100..101,
/// в котором нет ни одного прежнего сегмента.
fn routes() -> HashMap<String, Vec<Vec<u8>>> {
    let first_segment = decode_fixture(include_str!("fixtures/ts-0.ts.base64"));
    let second_segment = decode_fixture(include_str!("fixtures/ts-1.ts.base64"));
    HashMap::from([
        ("/master.m3u8".to_owned(), vec![live_master()]),
        (
            "/live.m3u8".to_owned(),
            vec![window(0), window(0), window(0), window(100)],
        ),
        ("/ts-0.ts".to_owned(), vec![first_segment.clone()]),
        ("/ts-1.ts".to_owned(), vec![second_segment.clone()]),
        ("/ts-100.ts".to_owned(), vec![first_segment]),
        ("/ts-101.ts".to_owned(), vec![second_segment]),
    ])
}

#[cfg(feature = "ffmpeg")]
#[test]
fn a_live_stream_that_lost_its_window_keeps_rendering_after_the_gap() {
    let server = ControlledHlsServer::start(routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeHlsUrl::new(
        server.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("16d3 native HLS live rejoin"),
    );
    let mut prepared = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    let PreparedNativeHlsLifecycle::Live { timeline_port } = &prepared.lifecycle else {
        panic!("a sliding playlist must open as live");
    };
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    // Засчитываются только кадры и PCM после старого края: они из окна «после сна».
    assert_decoder_render_audio_movement_after(
        prepared.demuxer.as_mut(),
        &mut wgpu_harness,
        OLD_LIVE_EDGE,
    );

    assert!(server.request_count("/ts-100.ts") >= 1);
    // Табло эфира продолжилось на той же шкале, за старым краем, а не упало.
    let live_edge = timeline_port
        .observe()
        .snapshot
        .state
        .as_live()
        .expect("the stream stays live after the gap")
        .live_edge()
        .as_duration();
    assert!(live_edge > OLD_LIVE_EDGE, "live edge {live_edge:?}");
    assert_eq!(process_spy.invocation_count(), 0);
}
