//! Договор `video-core`: player видит `Drained` только когда все кадры уже у него.
//!
//! На этом держится передача decoder-а новому описанию дорожки (сессия 03d моста):
//! player делает `flush` и перенастройку сразу после `Drained`, а `flush` отпускает
//! всё, что ещё лежит в канале кадров. Ранний `Drained` терял бы хвост эпизода.

use super::*;

// Собственный 16x16 красный кадр: FFmpeg lavfi color → libvpx-vp9, один IVF packet.
const RED_VP9_PACKET: &[u8] = &[
    130, 73, 131, 66, 0, 0, 240, 0, 246, 0, 56, 36, 28, 24, 74, 0, 0, 48, 96, 0, 0, 16, 191, 255,
    247, 29, 175, 255, 255, 255, 95, 223, 255, 255, 255, 242, 42, 192, 0,
];

/// Generation единственного эпизода теста.
const GENERATION: u64 = 3;

/// Сколько ждём, пока worker опубликует кадр и поставит `Drained`.
const WORKER_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn drained_is_reported_only_after_player_took_every_tail_frame() {
    let decoder_thread = FfmpegVideoDecoderThread::spawn(FfmpegDecoderThreadConfig::default())
        .expect("FFmpeg decoder thread should start");
    assert_eq!(
        decoder_thread.configure_stream(extradata_test_stream_config(
            codec_core::VideoCodec::Vp9,
            None,
            None,
        )),
        VideoStreamConfigResult::Configured
    );
    let mut packet = decode_packet_with_pts(GENERATION, 0, Duration::ZERO);
    packet.encoded_bytes = Bytes::from_static(RED_VP9_PACKET);
    packet.keyframe = true;
    decoder_thread
        .send_packet(packet)
        .expect("packet channel accepts the only packet");
    wait_until("packet ACK", || {
        decoder_thread.drain_completed_packet_count() == 1
    });

    // Кадр не забираем: worker может уже выжать decoder, но канал ещё полон.
    let begin = decoder_thread.begin_end_of_stream_drain(GENERATION);
    assert!(
        matches!(begin, VideoDecoderEndOfStreamDrainResult::Started(_)),
        "drain must start, got {begin:?}"
    );
    wait_until("decoded frame in channel", || {
        !decoder_thread.frame_rx.is_empty()
    });
    assert_eq!(
        decoder_thread.end_of_stream_drain_state(),
        VideoDecoderEndOfStreamDrainState::Draining {
            generation: GENERATION
        },
        "a frame still waits in the channel, so the decoder is not drained for the player"
    );

    let frame = decoder_thread
        .try_recv_frame()
        .expect("the tail frame reaches the player");
    assert_eq!(frame.generation, GENERATION);
    wait_until("drained after the player took the frame", || {
        decoder_thread.end_of_stream_drain_state()
            == VideoDecoderEndOfStreamDrainState::Drained {
                generation: GENERATION,
            }
    });
    decoder_thread.release_frame(frame.resource_handle);
}

/// Ждёт условие не дольше `WORKER_TIMEOUT`; worker живёт в своём потоке.
fn wait_until(waiting_for: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WORKER_TIMEOUT;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "FFmpeg worker did not reach: {waiting_for}"
        );
        std::thread::yield_now();
    }
}
