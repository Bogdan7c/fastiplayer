//! Сквозная проверка загрузки software-кадров без CPU-копии (cpu-parity 08b).
//!
//! Настоящий production FFmpeg decoder thread (VP9 — декодер с `AV_CODEC_CAP_DR1`, наш
//! `get_buffer2` кладёт кадры в блоки, выровненные по странице) → HostPlanar materializer
//! → `WgpuVideoRenderer` → `Queue::submit` → submitted release. Один и тот же поток
//! рисуется дважды:
//! - на устройстве приложения с `VK_EXT_external_memory_host` — кадры обязаны пройти без
//!   CPU-копии (счётчик materializer-а);
//! - на обычном устройстве wgpu — через staging, как раньше.
//!
//! Пиксели на экране обязаны совпасть кадр в кадр: прямая GPU-копия из памяти декодера
//! даёт ту же картинку, что и копия через staging.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]
#![cfg(feature = "vendored-ffmpeg")]

mod support;

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
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
use video_frame_contract::VideoFrameContract;

/// Фикстура `tests/fixtures/direct-frame-buffers/generate.sh`: VP9 176x100, 12 кадров, 5 fps.
const VP9_FIXTURE_RELATIVE_PATH: &str =
    "tests/fixtures/direct-frame-buffers/vp9-176x100-12frames.ivf";
const FIXTURE_FRAME_COUNT: usize = 12;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);
const VP9_TRACK_ID: TrackId = TrackId::new(1);

/// Режет IVF на пакеты-кадры (32-байтный заголовок файла, 12-байтный заголовок кадра).
fn read_vp9_ivf_packets() -> Vec<Bytes> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(VP9_FIXTURE_RELATIVE_PATH);
    let file_bytes = std::fs::read(&path).expect("read VP9 IVF fixture");
    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature");
    assert_eq!(&file_bytes[8..12], b"VP90", "IVF fourcc must be VP9");
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file_bytes.len() {
        let frame_len =
            u32::from_le_bytes(file_bytes[offset..offset + 4].try_into().expect("4 bytes"))
                as usize;
        let payload_start = offset + 12;
        packets.push(Bytes::copy_from_slice(
            &file_bytes[payload_start..payload_start + frame_len],
        ));
        offset = payload_start + frame_len;
    }
    packets
}

fn vp9_stream_config() -> VideoStreamDecodeConfig {
    VideoStreamDecodeConfig::from_requirement(
        VP9_TRACK_ID,
        &VideoDecodeRequirement::new(VideoCodec::Vp9),
        VideoFrameContract::host_yuv420_planar8(),
    )
}

/// Декодирует всю фикстуру, рисует каждый кадр и возвращает контрольные суммы экрана
/// по порядку кадров плюс счётчики путей загрузки.
fn render_every_frame(device_kind: OffscreenDeviceKind) -> (Vec<u64>, HostPlanarUploadPathStats) {
    let packets = read_vp9_ivf_packets();
    let mut harness = OffscreenWgpuHarness::with_device(device_kind);
    let (decoder, renderer_provider) = open_wrapped_decoder(vp9_stream_config(), &harness.queue);
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
                "VP9 frame at {:?} drawn",
                evidence.pts
            );
            checksums_by_pts.push((evidence.pts, evidence.visible_pixels_checksum));
        }
    };
    for (index, payload) in packets.iter().enumerate() {
        let pts = FIXTURE_FRAME_INTERVAL * u32::try_from(index).expect("index fits");
        let packet = DecodePacket {
            track_id: VP9_TRACK_ID,
            pts,
            dts: None,
            track_pts: None,
            track_dts: None,
            generation: generation.value(),
            encoded_bytes: payload.clone(),
            keyframe: index == 0,
            resolved_color: None,
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
        assert!(
            std::time::Instant::now() < deadline,
            "VP9 EOF drain timeout"
        );
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

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    let (direct_checksums, direct_stats) = render_every_frame(OffscreenDeviceKind::ProductionVideo);
    let (staged_checksums, staged_stats) = render_every_frame(OffscreenDeviceKind::PlainWgpu);

    assert_eq!(
        direct_checksums.len(),
        FIXTURE_FRAME_COUNT,
        "every frame reaches the screen"
    );
    assert_eq!(
        direct_stats.frames_without_cpu_copy, FIXTURE_FRAME_COUNT as u64,
        "every VP9 frame must be uploaded from decoder memory: {direct_stats:?}"
    );
    assert_eq!(direct_stats.frames_through_staging, 0, "{direct_stats:?}");
    assert_eq!(
        direct_stats.host_memory_import.import_failures, 0,
        "{direct_stats:?}"
    );
    // Импорт создаётся на каждую GPU-копию и уничтожается после неё.
    let import_stats = direct_stats.host_memory_import;
    assert_eq!(
        import_stats.imports_created, FIXTURE_FRAME_COUNT as u64,
        "{direct_stats:?}"
    );
    assert_eq!(
        staged_stats.frames_through_staging, FIXTURE_FRAME_COUNT as u64,
        "{staged_stats:?}"
    );
    assert_eq!(
        direct_checksums, staged_checksums,
        "direct GPU copy and staging copy must put the same pixels on screen"
    );
    eprintln!(
        "VP9_DIRECT_UPLOAD frames={} imports_created={}",
        direct_checksums.len(),
        direct_stats.host_memory_import.imports_created,
    );
}
