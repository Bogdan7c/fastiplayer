//! Сквозная проверка программного AV1 на закреплённом (vendored) FFmpeg.
//!
//! Встроенный декодер `av1` у FFmpeg работает только через аппаратное
//! ускорение, поэтому программный AV1 даёт `libdav1d`, который vendored-сборка
//! кладёт внутрь `libavcodec`. Эти тесты доказывают, что AV1 реально
//! декодируется в production decoder thread и доходит до WGPU submit — то
//! есть работает на машине, где нет ни VA-API, ни подходящего системного FFmpeg.

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
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::{TimeBase, TrackId, TrackTimestamp};
use render_wgpu_video::HostPlanarWgpuFrameMaterializer;
use support::wgpu_acceptance::{
    ACCEPTANCE_TIMEOUT, AcceptanceGeneration, OffscreenWgpuHarness,
    drain_decoder_to_eof_presentable, open_wrapped_decoder, send_decode_packet_and_collect_frames,
};
use video_backend_api::VideoBackendDecoderThreadHandle;
use video_core::{
    DecodePacket, DecodedFrame, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

/// Фикстура из `tests/fixtures/av1/generate.sh`: 160x90, 10 кадров, 5 fps, AV1 Main.
const AV1_FIXTURE_RELATIVE_PATH: &str = "tests/fixtures/av1/testsrc2-160x90-10frames.ivf";

/// Ожидаемые свойства фикстуры (совпадают с параметрами `generate.sh`).
const FIXTURE_WIDTH: u32 = 160;
const FIXTURE_HEIGHT: u32 = 90;
const FIXTURE_FRAME_COUNT: usize = 10;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);

/// Размер заголовка IVF-файла и заголовка каждого кадра (спецификация IVF).
const IVF_FILE_HEADER_BYTES: usize = 32;
const IVF_FRAME_HEADER_BYTES: usize = 12;

/// Track id фикстуры: в IVF всего один поток.
const AV1_TRACK_ID: TrackId = TrackId::new(1);

/// Один AV1 temporal unit из IVF вместе со временем показа.
struct Av1TemporalUnit {
    /// Время показа на media timeline.
    pts: Duration,
    /// То же время в единицах time base IVF — так его отдаёт настоящий демуксер.
    track_pts: TrackTimestamp,
    /// OBU-поток одного temporal unit.
    encoded_bytes: Bytes,
}

/// Читает IVF: проверяет сигнатуру/кодек и режет файл на temporal units.
fn read_av1_ivf_fixture() -> Vec<Av1TemporalUnit> {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(AV1_FIXTURE_RELATIVE_PATH);
    let file_bytes = std::fs::read(&fixture_path).expect("read AV1 IVF fixture");

    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature");
    assert_eq!(&file_bytes[8..12], b"AV01", "IVF fourcc must be AV1");
    let read_u32 = |offset: usize| {
        u32::from_le_bytes(file_bytes[offset..offset + 4].try_into().expect("4 bytes"))
    };
    // В IVF время кадра = timestamp * numerator / denominator секунд.
    let timebase_denominator = read_u32(16);
    let timebase_numerator = read_u32(20);
    let track_time_base =
        TimeBase::new(timebase_numerator, timebase_denominator).expect("valid IVF time base");

    let mut temporal_units = Vec::new();
    let mut frame_offset = IVF_FILE_HEADER_BYTES;
    while frame_offset < file_bytes.len() {
        let frame_size = usize::try_from(read_u32(frame_offset)).expect("frame size fits");
        let timestamp = u64::from_le_bytes(
            file_bytes[frame_offset + 4..frame_offset + 12]
                .try_into()
                .expect("8 bytes"),
        );
        let payload_start = frame_offset + IVF_FRAME_HEADER_BYTES;
        let payload_end = payload_start + frame_size;
        temporal_units.push(Av1TemporalUnit {
            pts: Duration::from_micros(
                timestamp * u64::from(timebase_numerator) * 1_000_000
                    / u64::from(timebase_denominator),
            ),
            track_pts: TrackTimestamp::new(
                AV1_TRACK_ID,
                i64::try_from(timestamp).expect("IVF timestamp fits"),
                track_time_base,
            ),
            encoded_bytes: Bytes::copy_from_slice(&file_bytes[payload_start..payload_end]),
        });
        frame_offset = payload_end;
    }
    temporal_units
}

/// AV1 8-bit 4:2:0 выводится в обычный HostPlanar YUV420P8 контракт.
fn av1_stream_config() -> VideoStreamDecodeConfig {
    VideoStreamDecodeConfig::from_requirement(
        AV1_TRACK_ID,
        &VideoDecodeRequirement::new(VideoCodec::Av1),
        VideoFrameContract::host_yuv420_planar8(),
    )
}

/// Строит decoder packet; ключевой кадр — первый temporal unit (в нём sequence header).
fn av1_decode_packet(
    temporal_unit: &Av1TemporalUnit,
    unit_index: usize,
    generation: AcceptanceGeneration,
) -> DecodePacket {
    DecodePacket {
        track_id: AV1_TRACK_ID,
        pts: temporal_unit.pts,
        dts: None,
        track_pts: Some(temporal_unit.track_pts),
        track_dts: None,
        generation: generation.value(),
        encoded_bytes: temporal_unit.encoded_bytes.clone(),
        keyframe: unit_index == 0,
        resolved_color: None,
    }
}

/// Запускает EOF drain и собирает все хвостовые кадры до состояния `Drained`.
fn drain_all_remaining_frames(
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
) -> Vec<DecodedFrame> {
    let begin_result = decoder.begin_end_of_stream_drain(generation.value());
    assert!(
        matches!(begin_result, VideoDecoderEndOfStreamDrainResult::Started(_)),
        "decoder must accept EOF drain, got {begin_result:?}"
    );

    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    let mut tail_frames = Vec::new();
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            tail_frames.push(frame);
        }
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained { .. } => break,
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("AV1 EOF drain ended in error: {error}")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "AV1 EOF drain timeout");
        thread::sleep(Duration::from_millis(1));
    }
    // Кадры, опубликованные вместе с последним wake-up.
    while let Some(frame) = decoder.try_recv_frame() {
        tail_frames.push(frame);
    }
    tail_frames
}

/// Размер и время декодированного кадра; сам кадр сразу возвращается декодеру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedFrameFacts {
    width: u32,
    height: u32,
    pts: Duration,
}

/// Запоминает факты о кадрах и сразу освобождает их: пул HostPlanar-кадров
/// ограничен, и удержание всех кадров остановило бы декодер (backpressure).
fn record_and_release(
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
    frames: Vec<DecodedFrame>,
    recorded_facts: &mut Vec<DecodedFrameFacts>,
) {
    for frame in frames {
        assert_eq!(frame.generation, generation.value(), "no stale generation");
        recorded_facts.push(DecodedFrameFacts {
            width: frame.width,
            height: frame.height,
            pts: frame.pts,
        });
        decoder.release_frame(frame.resource_handle);
    }
}

/// Без GPU: каждый temporal unit фикстуры становится HostPlanar-кадром нужного размера.
#[test]
fn av1_fixture_decodes_every_frame_in_software_on_vendored_ffmpeg() {
    let temporal_units = read_av1_ivf_fixture();
    assert_eq!(
        temporal_units.len(),
        FIXTURE_FRAME_COUNT,
        "fixture frame count"
    );

    let decoder = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("start software FFmpeg backend")
        .into_decoder_thread();
    assert_eq!(
        decoder.configure_stream(av1_stream_config()),
        VideoStreamConfigResult::Configured
    );

    let generation = AcceptanceGeneration::BeforeSeek;
    let mut decoded_facts = Vec::new();
    for (unit_index, temporal_unit) in temporal_units.iter().enumerate() {
        let packet = av1_decode_packet(temporal_unit, unit_index, generation);
        let frames = send_decode_packet_and_collect_frames(decoder.as_ref(), packet);
        record_and_release(decoder.as_ref(), generation, frames, &mut decoded_facts);
    }
    let tail_frames = drain_all_remaining_frames(decoder.as_ref(), generation);
    record_and_release(
        decoder.as_ref(),
        generation,
        tail_frames,
        &mut decoded_facts,
    );

    // Ни один кадр не потерян, и каждый имеет размер и время из фикстуры.
    let expected_facts: Vec<DecodedFrameFacts> = (0..FIXTURE_FRAME_COUNT)
        .map(|frame_index| DecodedFrameFacts {
            width: FIXTURE_WIDTH,
            height: FIXTURE_HEIGHT,
            pts: FIXTURE_FRAME_INTERVAL * u32::try_from(frame_index).expect("index fits"),
        })
        .collect();
    assert_eq!(decoded_facts, expected_facts);
}

/// С GPU: первый и последний AV1-кадры проходят materializer, renderer и `Queue::submit`.
#[test]
#[ignore = "requires a Vulkan adapter (lavapipe is enough)"]
fn av1_fixture_reaches_wgpu_submit_and_release_on_vendored_ffmpeg() {
    let temporal_units = read_av1_ivf_fixture();

    let mut wgpu_harness = OffscreenWgpuHarness::new();
    let (decoder, renderer_provider) =
        open_wrapped_decoder(av1_stream_config(), &wgpu_harness.queue);
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        &wgpu_harness.device,
        &wgpu_harness.queue,
        renderer_provider.clone(),
    );

    let generation = AcceptanceGeneration::BeforeSeek;
    let mut first_frame = None;
    let mut first_evidence = None;
    let mut last_frame: Option<DecodedFrame> = None;
    for (unit_index, temporal_unit) in temporal_units.iter().enumerate() {
        let packet = av1_decode_packet(temporal_unit, unit_index, generation);
        for frame in send_decode_packet_and_collect_frames(decoder.as_ref(), packet) {
            if first_frame.is_none() && first_evidence.is_none() {
                first_frame = Some(frame);
                continue;
            }
            // Держим только самый поздний кадр; предыдущий кандидат возвращаем декодеру.
            if let Some(previous_last) = last_frame.replace(frame) {
                decoder.release_frame(previous_last.resource_handle);
            }
        }
        // Первый кадр рисуем сразу, чтобы не держать его в ограниченном пуле.
        if let Some(frame) = first_frame.take() {
            assert_eq!(frame.pts, Duration::ZERO);
            first_evidence =
                Some(wgpu_harness.submit_and_release(&materializer, &renderer_provider, frame));
        }
    }
    let first_evidence = first_evidence.expect("AV1 decoder must publish the first frame");
    assert!(
        first_evidence.contains_visible_video,
        "first AV1 frame drawn"
    );

    // libdav1d может отдать последний кадр как во время подачи, так и из EOF drain-а.
    let last_frame_pts =
        FIXTURE_FRAME_INTERVAL * u32::try_from(FIXTURE_FRAME_COUNT - 1).expect("frame count fits");
    let last_frame = match last_frame {
        Some(frame) if frame.pts == last_frame_pts => {
            // Хвоста быть не должно, но EOF drain обязан корректно завершиться.
            for tail_frame in drain_all_remaining_frames(decoder.as_ref(), generation) {
                decoder.release_frame(tail_frame.resource_handle);
            }
            frame
        }
        earlier_frame => {
            if let Some(frame) = earlier_frame {
                decoder.release_frame(frame.resource_handle);
            }
            drain_decoder_to_eof_presentable(decoder.as_ref(), generation, last_frame_pts)
                .expect("EOF drain must publish the last AV1 frame")
        }
    };
    let last_evidence =
        wgpu_harness.submit_and_release(&materializer, &renderer_provider, last_frame);
    assert_eq!(last_evidence.pts, last_frame_pts);
    assert!(last_evidence.contains_visible_video, "last AV1 frame drawn");

    // Маркер для CI и аудитора: AV1 прошёл полный путь до submit/release.
    eprintln!(
        "AV1_SOFTWARE_WGPU first_pts_us={} last_pts_us={} decoder=vendored-libdav1d submit=completed release=completed",
        first_evidence.pts.as_micros(),
        last_evidence.pts.as_micros(),
    );
}
