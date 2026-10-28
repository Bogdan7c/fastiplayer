//! Сквозная проверка загрузки software-кадров без CPU-копии (cpu-parity 08b).
//!
//! Настоящий production FFmpeg decoder thread (декодер с `AV_CODEC_CAP_DR1`, наш
//! `get_buffer2` кладёт кадры в блоки, выровненные по странице) → HostPlanar materializer
//! → `WgpuVideoRenderer` → `Queue::submit` → submitted release. Один и тот же поток
//! рисуется дважды:
//! - на устройстве приложения с `VK_EXT_external_memory_host` — кадры обязаны пройти без
//!   CPU-копии (счётчик materializer-а);
//! - на обычном устройстве wgpu — через staging, как раньше.
//!
//! Пиксели на экране обязаны совпасть кадр в кадр: прямая GPU-копия из памяти декодера
//! даёт ту же картинку, что и копия через staging.
//!
//! Потоки — вся матрица «кодек × битность × subsampling», которую играет программный путь
//! плеера (сессия 13a плана gles2-renderer): H.264, HEVC (Main/Main10/Main12, 4:2:2 10/12,
//! 4:4:4 8/10), VP8, VP9 (профили 0–3), AV1 Main и High (libdav1d получил `DR1` патчем
//! `ffmpeg-vendor`, сессия 13). Каталог клеток — `fixtures.rs`.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]
#![cfg(feature = "vendored-ffmpeg")]

#[path = "../support/mod.rs"]
mod support;

mod fixtures;

use std::time::Duration;

use codec_core::VideoDecodeRequirement;
use fixtures::FixtureStream;
use media_core::TrackId;
use render_wgpu_video::{HostPlanarUploadPathStats, HostPlanarWgpuFrameMaterializer};
use support::wgpu_acceptance::{
    AcceptanceGeneration, OffscreenDeviceKind, OffscreenWgpuHarness, open_wrapped_decoder,
    send_decode_packet_and_collect_frames,
};
use video_core::{
    DecodePacket, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoStreamDecodeConfig,
};

/// У каждой фикстуры (`tests/fixtures/direct-frame-buffers/generate.sh`) 12 кадров, 5 fps.
const FIXTURE_FRAME_COUNT: usize = 12;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);
const FIXTURE_TRACK_ID: TrackId = TrackId::new(1);

fn stream_config(stream: &FixtureStream) -> VideoStreamDecodeConfig {
    VideoStreamDecodeConfig::from_requirement(
        FIXTURE_TRACK_ID,
        &VideoDecodeRequirement::new(stream.codec),
        stream.contract,
    )
}

/// Декодирует всю фикстуру, рисует каждый кадр и возвращает контрольные суммы экрана
/// по порядку кадров плюс счётчики путей загрузки.
fn render_every_frame(
    stream: &FixtureStream,
    device_kind: OffscreenDeviceKind,
) -> (Vec<u64>, HostPlanarUploadPathStats) {
    let packets = stream.read_packets();
    let mut harness = OffscreenWgpuHarness::with_device(device_kind);
    let (decoder, renderer_provider) = open_wrapped_decoder(stream_config(stream), &harness.queue);
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        &harness.device,
        &harness.queue,
        renderer_provider.clone(),
    );
    let generation = AcceptanceGeneration::BeforeSeek;

    let mut checksums_by_pts = Vec::new();
    let mut render = |frames: Vec<video_core::DecodedFrame>, harness: &mut OffscreenWgpuHarness| {
        for frame in frames {
            let evidence = harness.submit_and_release(&materializer, &renderer_provider, frame);
            assert!(
                evidence.contains_visible_video,
                "{} frame at {:?} drawn",
                stream.file_name, evidence.pts
            );
            checksums_by_pts.push((evidence.pts, evidence.visible_pixels_checksum));
        }
    };
    for (index, payload) in packets.iter().enumerate() {
        let pts = FIXTURE_FRAME_INTERVAL * u32::try_from(index).expect("index fits");
        let packet = DecodePacket {
            track_id: FIXTURE_TRACK_ID,
            pts,
            dts: None,
            track_pts: None,
            track_dts: None,
            generation: generation.value(),
            encoded_bytes: payload.clone(),
            keyframe: index == 0,
            resolved_color: stream.resolved_color.clone(),
        };
        let frames = send_decode_packet_and_collect_frames(decoder.as_ref(), packet);
        render(frames, &mut harness);
    }
    assert!(matches!(
        decoder.begin_end_of_stream_drain(generation.value()),
        VideoDecoderEndOfStreamDrainResult::Started(_)
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut tail = Vec::new();
        while let Some(frame) = decoder.try_recv_frame() {
            tail.push(frame);
        }
        render(tail, &mut harness);
        if matches!(
            decoder.end_of_stream_drain_state(),
            VideoDecoderEndOfStreamDrainState::Drained { .. }
        ) {
            let mut last = Vec::new();
            while let Some(frame) = decoder.try_recv_frame() {
                last.push(frame);
            }
            render(last, &mut harness);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "EOF drain timeout");
        std::thread::sleep(Duration::from_millis(1));
    }

    drop(render);
    checksums_by_pts.sort_by_key(|(pts, _)| *pts);
    let checksums = checksums_by_pts
        .into_iter()
        .map(|(_, checksum)| checksum)
        .collect();
    (checksums, materializer.upload_path_stats())
}

/// Рисует поток на устройстве приложения (импорт памяти декодера) и на обычном wgpu
/// (staging) и требует: все кадры без CPU-копии, импорт на каждую копию, пиксели те же.
fn assert_frames_reach_screen_from_decoder_memory(stream: &FixtureStream) {
    let (direct_checksums, direct_stats) =
        render_every_frame(stream, OffscreenDeviceKind::ProductionVideo);
    let (staged_checksums, staged_stats) =
        render_every_frame(stream, OffscreenDeviceKind::PlainWgpu);
    let name = stream.file_name;

    assert_eq!(
        direct_checksums.len(),
        FIXTURE_FRAME_COUNT,
        "{name}: every frame reaches the screen"
    );
    assert_eq!(
        direct_stats.frames_without_cpu_copy, FIXTURE_FRAME_COUNT as u64,
        "{name}: every frame must be uploaded from decoder memory: {direct_stats:?}"
    );
    assert_eq!(
        direct_stats.frames_through_staging, 0,
        "{name}: {direct_stats:?}"
    );
    assert_eq!(
        direct_stats.host_memory_import.import_failures, 0,
        "{name}: {direct_stats:?}"
    );
    // Импорт создаётся на каждую GPU-копию и уничтожается после неё.
    let import_stats = direct_stats.host_memory_import;
    assert_eq!(
        import_stats.imports_created, FIXTURE_FRAME_COUNT as u64,
        "{name}: {direct_stats:?}"
    );
    assert_eq!(
        staged_stats.frames_through_staging, FIXTURE_FRAME_COUNT as u64,
        "{name}: {staged_stats:?}"
    );
    assert_eq!(
        direct_checksums, staged_checksums,
        "{name}: direct GPU copy and staging copy must put the same pixels on screen"
    );
    eprintln!(
        "DIRECT_UPLOAD {name} frames={} imports_created={}",
        direct_checksums.len(),
        direct_stats.host_memory_import.imports_created,
    );
}

/// 10-битный поток без цветовых тегов (ни в контейнере, ни в битстриме) рисуется как
/// SDR BT.709 limited — ровно так же, как тот же поток с явным BT.709. Раньше такой кадр
/// шёл с цветом «Unknown», и рендер 10/12 бит падал фатальной ошибкой (сессия 13a).
fn assert_untagged_high_bit_stream_draws_like_bt709(tagged: FixtureStream) {
    let name = tagged.file_name;
    let untagged = tagged.same_cell().without_packet_color();
    let (tagged_checksums, _) = render_every_frame(&tagged, OffscreenDeviceKind::ProductionVideo);
    let (untagged_checksums, untagged_stats) =
        render_every_frame(&untagged, OffscreenDeviceKind::ProductionVideo);

    assert_eq!(
        untagged_checksums.len(),
        FIXTURE_FRAME_COUNT,
        "{name}: every untagged frame reaches the screen"
    );
    assert_eq!(
        untagged_stats.frames_without_cpu_copy, FIXTURE_FRAME_COUNT as u64,
        "{name}: {untagged_stats:?}"
    );
    assert_eq!(
        untagged_checksums, tagged_checksums,
        "{name}: untagged 10-bit must look exactly like BT.709 limited"
    );
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_10bit_without_color_tags_draws_like_bt709() {
    assert_untagged_high_bit_stream_draws_like_bt709(FixtureStream::vp9_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_10bit_without_color_tags_draws_like_bt709() {
    assert_untagged_high_bit_stream_draws_like_bt709(FixtureStream::av1_10bit());
}

// H.264: любой профиль плеер играет как 8 бит 4:2:0.

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn h264_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::h264());
}

// HEVC: Main, Main10, Main12, Main 4:2:2 10/12, Main 4:4:4 8/10.

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_main_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_main());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_main10_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_main10());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_main12_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_main12());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_422_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_422_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_422_12bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_422_12bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_444_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_444());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn hevc_444_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::hevc_444_10bit());
}

// VP8: 8 бит 4:2:0.

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp8_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp8());
}

// VP9: профили 0–3 (4:4:4 12 бит плеер не играет).

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_422_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_422());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_444_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_444());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_12bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_12bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_422_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_422_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_422_12bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_422_12bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_444_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9_444_10bit());
}

// AV1: Main (4:2:0 8/10 бит, с зерном плёнки) и High (4:4:4 8/10 бит).

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_film_grain_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_film_grain());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_444_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_444());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_444_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_444_10bit());
}
