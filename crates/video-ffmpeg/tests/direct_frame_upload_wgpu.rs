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
//! Потоки: VP9 (cpu-parity 08b) и AV1 8/10 бит и с зерном плёнки — libdav1d получил `DR1`
//! патчем `ffmpeg-vendor` (сессия 13 плана gles2-renderer). Без патча кадры AV1 лежат в
//! пуле FFmpeg, и `frames_without_cpu_copy` остаётся 0.

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
use codec_core::{VideoCodec, VideoColorMetadata, VideoDecodeRequirement};
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

/// Фикстуры `tests/fixtures/direct-frame-buffers/generate.sh`: 176x100, 12 кадров, 5 fps.
const FIXTURE_DIRECTORY: &str = "tests/fixtures/direct-frame-buffers";
const FIXTURE_FRAME_COUNT: usize = 12;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);
const FIXTURE_TRACK_ID: TrackId = TrackId::new(1);

/// Поток фикстуры: кодек, файл IVF и контракт кадра хоста.
struct FixtureStream {
    codec: VideoCodec,
    /// Код кодека в заголовке IVF (`VP90`, `AV01`).
    ivf_fourcc: [u8; 4],
    ivf_file_name: &'static str,
    contract: VideoFrameContract,
    /// 10-битному плану рисования цвет обязателен (в сыром IVF его нет).
    resolved_color: Option<VideoColorMetadata>,
}

impl FixtureStream {
    fn eight_bit(codec: VideoCodec, ivf_fourcc: [u8; 4], ivf_file_name: &'static str) -> Self {
        Self {
            codec,
            ivf_fourcc,
            ivf_file_name,
            contract: VideoFrameContract::host_yuv420_planar8(),
            resolved_color: None,
        }
    }

    fn vp9() -> Self {
        Self::eight_bit(VideoCodec::Vp9, *b"VP90", "vp9-176x100-12frames.ivf")
    }

    fn av1() -> Self {
        Self::eight_bit(VideoCodec::Av1, *b"AV01", "av1-176x100-12frames.ivf")
    }

    fn av1_film_grain() -> Self {
        Self::eight_bit(
            VideoCodec::Av1,
            *b"AV01",
            "av1-filmgrain-176x100-12frames.ivf",
        )
    }

    fn av1_10bit() -> Self {
        Self {
            codec: VideoCodec::Av1,
            ivf_fourcc: *b"AV01",
            ivf_file_name: "av1-10bit-176x100-12frames.ivf",
            contract: VideoFrameContract::host_yuv420_planar10le(),
            resolved_color: Some(VideoColorMetadata::sdr_bt709_limited()),
        }
    }
}

/// Режет IVF на пакеты-кадры (32-байтный заголовок файла, 12-байтный заголовок кадра).
fn read_ivf_packets(stream: &FixtureStream) -> Vec<Bytes> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIRECTORY)
        .join(stream.ivf_file_name);
    let file_bytes = std::fs::read(&path).expect("read IVF fixture");
    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature");
    assert_eq!(
        &file_bytes[8..12],
        &stream.ivf_fourcc,
        "IVF fourcc of {}",
        stream.ivf_file_name
    );
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
    let packets = read_ivf_packets(stream);
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
                stream.ivf_file_name, evidence.pts
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
    let name = stream.ivf_file_name;

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

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn vp9_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::vp9());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_10bit_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_10bit());
}

#[test]
#[ignore = "requires a Vulkan adapter with VK_EXT_external_memory_host"]
fn av1_film_grain_frames_reach_screen_from_decoder_memory_without_cpu_copy() {
    assert_frames_reach_screen_from_decoder_memory(&FixtureStream::av1_film_grain());
}
