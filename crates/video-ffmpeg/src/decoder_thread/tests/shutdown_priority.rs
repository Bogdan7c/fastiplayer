//! Уже известный shutdown останавливает настоящий worker до обработки queued media.

use super::*;
use crossbeam_channel::TryRecvError;

#[derive(Clone, Copy)]
enum ShutdownSignal {
    Requested,
    Disconnected,
}

#[test]
fn queued_shutdown_preempts_packet_processing_and_releases_worker_publishers() {
    assert_shutdown_before_worker_start(ShutdownSignal::Requested);
}

#[test]
fn disconnected_shutdown_preempts_packet_processing_and_releases_worker_publishers() {
    assert_shutdown_before_worker_start(ShutdownSignal::Disconnected);
}

fn assert_shutdown_before_worker_start(signal: ShutdownSignal) {
    let (packet_tx, packet_rx) = bounded(1);
    let queued_packets = packet_rx.clone();
    packet_tx
        .send(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("queue real decode packet");
    let (control_tx, control_rx) = bounded(1);
    let (shutdown_tx, shutdown_rx) = bounded(1);
    match signal {
        ShutdownSignal::Requested => shutdown_tx.send(()).expect("queue shutdown before spawn"),
        ShutdownSignal::Disconnected => {}
    }
    drop(shutdown_tx);
    let (frame_tx, frame_rx) = bounded(1);
    let (error_tx, error_rx) = bounded(1);
    let completions = Arc::new(FfmpegPacketCompletionCounter::default());
    let worker_completions = completions.clone();
    let (terminated_tx, terminated_rx) = bounded(1);
    let thread = std::thread::spawn(move || {
        // FFmpeg owners создаются на worker thread, как в production spawn.
        let (release_tx, release_notify_rx) = bounded(1);
        let resource_provider = FfmpegHostResourceProvider::new(1, release_tx);
        let (activity_notifier, _subscription) = VideoDecoderActivityNotifier::new();
        let (full_pool_wait_observer_tx, _pool_observer) = bounded(1);
        let worker = FfmpegDecoderWorker {
            active_decoder: None,
            activity_notifier,
            eof_drain_state: shared_idle_drain_state(),
            frame_tx,
            resource_provider,
            release_notify_rx,
            pending_packet: None,
            pending_eof_drain_generation: None,
            packet_completion_counter: worker_completions,
            error_tx,
            software_decode_thread_budget: VideoDecoderThreadConfig::default()
                .software_decode_thread_budget,
            full_pool_wait_observer_tx,
        };
        worker.run(packet_rx, control_rx, shutdown_rx);
        terminated_tx
            .send(())
            .expect("publish actual worker termination");
    });
    terminated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("pre-signalled worker must terminate");
    thread.join().expect("join terminated worker");
    assert_eq!(
        queued_packets.len(),
        1,
        "shutdown must preserve the queued packet"
    );
    assert_eq!(
        completions.drain(),
        0,
        "unprocessed packet has no decode completion"
    );
    assert!(matches!(
        frame_rx.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
    assert!(matches!(
        error_rx.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
    drop(control_tx);
    drop(packet_tx);
}
