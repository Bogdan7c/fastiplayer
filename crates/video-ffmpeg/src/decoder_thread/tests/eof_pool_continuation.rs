//! Настоящий FFmpeg tail frame проходит worker, full pool и renderer resource lookup.

use super::*;
use video_backend_api::PresentFrameResourceProvider;

// Собственный 16x16 красный кадр: FFmpeg lavfi color → libvpx-vp9, один IVF packet.
const RED_VP9_PACKET: &[u8] = &[
    130, 73, 131, 66, 0, 0, 240, 0, 246, 0, 56, 36, 28, 24, 74, 0, 0, 48, 96, 0, 0, 16, 191, 255,
    247, 29, 175, 255, 255, 255, 95, 223, 255, 255, 255, 242, 42, 192, 0,
];

#[test]
fn full_pool_release_resumes_real_eof_frame_then_reaches_terminal_drain() {
    let (packet_tx, packet_rx) = bounded(1);
    let (control_tx, control_rx) = bounded(1);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let (frame_tx, frame_rx) = bounded(1);
    let (error_tx, error_rx) = bounded(1);
    let (ready_tx, ready_rx) = bounded(1);
    let (pool_wait_tx, pool_wait_rx) = bounded(1);
    let (terminated_tx, terminated_rx) = bounded(1);
    let drain_state = shared_idle_drain_state();
    let observed_drain_state = drain_state.clone();
    let thread = std::thread::spawn(move || {
        let (release_tx, release_notify_rx) = bounded(1);
        let provider = FfmpegHostResourceProvider::new(1, release_tx);
        let held = provider
            .insert_frame(
                1,
                test_yuv420_frame(16, 16, 32),
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect("reserve the only presentation slot");
        let (notifier, _subscription) = VideoDecoderActivityNotifier::new();
        let config = extradata_test_stream_config(codec_core::VideoCodec::Vp9, None, None);
        let budget = VideoDecoderThreadConfig::default().software_decode_thread_budget;
        let api = RealFfmpegDecodeApi::open(&config, budget).expect("open real VP9 decoder");
        let mut decode_loop =
            SendReceiveDecodeLoop::new(api, notifier.clone(), drain_state.clone());
        let mut packet = decode_packet_with_pts(1, 0, Duration::ZERO);
        packet.encoded_bytes = Bytes::from_static(RED_VP9_PACKET);
        // Нулевой receive budget сохраняет настоящий decoded frame внутри FFmpeg.
        assert!(matches!(
            decode_loop
                .send_packet(packet, 0)
                .expect("prime VP9 packet"),
            SendPacketOutcome::Consumed(_)
        ));
        ready_tx
            .send((provider.clone(), held))
            .expect("publish renderer owner");
        FfmpegDecoderWorker {
            active_decoder: Some(ConfiguredFfmpegDecoder {
                config,
                decode_loop,
            }),
            activity_notifier: notifier,
            eof_drain_state: drain_state,
            frame_tx,
            resource_provider: provider,
            release_notify_rx,
            pending_packet: None,
            pending_eof_drain_generation: Some(1),
            packet_completion_counter: Arc::new(FfmpegPacketCompletionCounter::default()),
            error_tx,
            software_decode_thread_budget: budget,
            full_pool_wait_observer_tx: pool_wait_tx,
        }
        .run(packet_rx, control_rx, shutdown_rx);
        terminated_tx
            .send(())
            .expect("publish joined lifecycle outcome");
    });
    let (provider, held) = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("decoder ready");
    pool_wait_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker observes full pool");
    provider.release_frame(held.handle);
    let frame = frame_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("EOF frame reaches presentation");
    assert_eq!(frame.generation, 1);
    // Этот второй ack возможен только после owner reentry с заполненным output pool.
    pool_wait_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("tail frame fills pool again");
    let descriptor = lookup_host_planar_descriptor(&provider, frame.resource_handle);
    assert_eq!(
        descriptor
            .visible_plane_row_bytes(0, 0)
            .expect("visible luma")
            .len(),
        16
    );
    provider.release_frame(frame.resource_handle);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if matches!(
            *observed_drain_state.lock().expect("drain state"),
            VideoDecoderEndOfStreamDrainState::Drained { generation: 1 }
        ) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "last release must finish EOF"
        );
        std::thread::yield_now();
    }
    assert!(error_rx.is_empty());
    assert_eq!(provider.free_slots(), 1);
    shutdown_tx.send(()).expect("request shutdown");
    terminated_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker terminates");
    thread.join().expect("worker must not panic");
    drop((packet_tx, control_tx));
}
