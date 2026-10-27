//! Вид памяти блоков кадров доходит от `FfmpegDecoderThreadConfig` до настоящего декодера.
//!
//! Проверяется весь путь: config → worker → `RealFfmpegDecodeApi::open` → пул блоков →
//! декодированный кадр в таблице ресурсов. Доказательство — у кадра, который видит
//! renderer, `sealed_memfd()` есть (memfd) либо нет (анонимная память).

use super::*;
use video_backend_api::PresentFrameResourceProvider;

// Тот же красный 16x16 кадр VP9, что в `eof_pool_continuation`.
const RED_VP9_PACKET: &[u8] = &[
    130, 73, 131, 66, 0, 0, 240, 0, 246, 0, 56, 36, 28, 24, 74, 0, 0, 48, 96, 0, 0, 16, 191, 255,
    247, 29, 175, 255, 255, 255, 95, 223, 255, 255, 255, 242, 42, 192, 0,
];

/// Декодирует один VP9-кадр потоком с заданным видом памяти; возвращает `sealed_memfd().is_some()`
/// у блока кадра (или `None`, если у кадра нет импортируемого блока вовсе).
fn decoded_frame_block_is_memfd(memory: HostFrameMemoryKind) -> Option<bool> {
    let config = FfmpegDecoderThreadConfig::default().with_host_frame_memory(memory);
    let decoder = FfmpegVideoDecoderThread::spawn(config).expect("start FFmpeg decoder thread");
    let stream_config = extradata_test_stream_config(codec_core::VideoCodec::Vp9, None, None);
    assert_eq!(
        decoder.configure_stream(stream_config),
        VideoStreamConfigResult::Configured
    );
    let mut packet = decode_packet_with_pts(1, 0, Duration::ZERO);
    packet.encoded_bytes = Bytes::from_static(RED_VP9_PACKET);
    decoder.send_packet(packet).expect("send VP9 packet");
    // Один пакет VP9 выходит из декодера только после дренажа конца потока.
    decoder.begin_end_of_stream_drain(1);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = decoder.try_recv_frame() {
            break frame;
        }
        assert!(
            decoder.try_recv_error().is_none(),
            "decoder reported an error"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "no decoded frame in 5 s"
        );
        std::thread::sleep(Duration::from_millis(5));
    };

    let descriptor =
        lookup_host_planar_descriptor(&decoder.host_resource_provider, frame.resource_handle);
    let memory = descriptor
        .importable_memory()
        .expect("importable memory lookup")?;
    let is_memfd = memory.block().sealed_memfd().is_some();
    decoder
        .host_resource_provider
        .release_frame(frame.resource_handle);
    Some(is_memfd)
}

#[test]
fn memfd_config_decodes_real_frame_into_memfd_block() {
    assert_eq!(
        decoded_frame_block_is_memfd(HostFrameMemoryKind::SealedMemfd),
        Some(true)
    );
}

#[test]
fn default_config_decodes_real_frame_into_anonymous_block() {
    assert_eq!(
        decoded_frame_block_is_memfd(HostFrameMemoryKind::Anonymous),
        Some(false)
    );
}
