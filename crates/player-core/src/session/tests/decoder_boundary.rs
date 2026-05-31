use super::test_support::*;
use super::*;

#[test]
fn playback_pipeline_decoder_boundary_absent_thread_is_noop() {
    let pipeline = PlaybackPipeline::default();

    assert!(!pipeline.has_active_video_decoder());
    assert_eq!(pipeline.video_backend_name(), "Synthetic (test)");
    assert!(pipeline.flush_video_decoder_thread().is_ok());
    assert!(pipeline.try_recv_decoded_video_frame().is_none());
    assert!(pipeline.try_recv_video_decoder_diagnostic_event().is_none());
    assert!(pipeline.try_recv_video_decoder_error().is_none());
    assert_eq!(pipeline.drain_completed_video_decode_packet_count(), 0);
    assert!(pipeline.video_decoder_control_channel_pressure().is_none());
    assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
    assert!(!pipeline.can_send_video_decode_packets());
    assert!(!pipeline.can_receive_decoded_video_frames());
    assert!(
        pipeline
            .send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(10)))
            .is_none()
    );
    assert!(!pipeline.release_frame_to_video_decoder(video_core::FrameResourceHandle(7)));
}

#[test]
fn playback_pipeline_decoder_boundary_forwards_send_results() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let backpressure_reason = DecodeBackpressureReason::PacketQueueFull {
        queued_packets: 4,
        capacity: 4,
    };
    fake_decoder.push_send_result(Ok(()));
    fake_decoder.push_send_result(Err(DecodeSendError::Backpressure(backpressure_reason)));
    fake_decoder.push_send_result(Err(DecodeSendError::Fatal(DecodeThreadError::new(
        "fatal send failure",
    ))));
    pipeline.set_video_decoder_thread(fake_decoder);

    assert!(pipeline.has_active_video_decoder());
    assert_eq!(pipeline.video_backend_name(), "Shared fake decoder");
    assert!(pipeline.can_send_video_decode_packets());
    assert!(matches!(
        pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(1))),
        Some(Ok(()))
    ));

    match pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(2))) {
        Some(Err(DecodeSendError::Backpressure(reason))) => {
            assert_eq!(reason, backpressure_reason);
        }
        unexpected_result => {
            panic!("expected decoder backpressure, got {unexpected_result:?}");
        }
    }

    match pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(3))) {
        Some(Err(DecodeSendError::Fatal(error))) => {
            assert_eq!(error.message(), "fatal send failure");
        }
        unexpected_result => {
            panic!("expected fatal decoder send error, got {unexpected_result:?}");
        }
    }
}

#[test]
fn playback_pipeline_decoder_boundary_drains_acks_without_touching_in_flight() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    fake_decoder.add_completed_packet_count(2);
    pipeline.set_video_decoder_thread(fake_decoder);
    pipeline.note_video_packet_sent_to_decoder();
    pipeline.note_video_packet_sent_to_decoder();
    pipeline.note_video_packet_sent_to_decoder();

    assert_eq!(pipeline.video_decode_in_flight_packets(), 3);

    let completed_packet_count = pipeline.drain_completed_video_decode_packet_count();

    assert_eq!(completed_packet_count, 2);
    assert_eq!(pipeline.video_decode_in_flight_packets(), 3);
    assert_eq!(pipeline.drain_completed_video_decode_packet_count(), 0);

    pipeline.note_video_packets_completed_by_decoder(completed_packet_count);

    assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
}

#[test]
fn playback_pipeline_decoder_boundary_forwards_diagnostic_events() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let pressure = video_core::VideoFramePublishPressureDiagnostics {
        frame_publish_channel_full_count: 2,
        pending_publish_retry_count: 1,
        max_decoded_frame_publish_latency: Duration::from_millis(8),
        total_decoded_frame_publish_latency: Duration::from_millis(8),
    };
    fake_decoder.push_diagnostic_event(
        video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure },
    );
    pipeline.set_video_decoder_thread(fake_decoder);

    let event = pipeline
        .try_recv_video_decoder_diagnostic_event()
        .expect("fake decoder diagnostic event should cross pipeline boundary");

    assert_eq!(
        event,
        video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure }
    );
    assert!(pipeline.try_recv_video_decoder_diagnostic_event().is_none());
}

#[test]
fn playback_pipeline_decoder_boundary_forwards_control_pressure_snapshot() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let pressure = DecoderControlChannelPressureSnapshot {
        control_channel_len: 31,
        control_channel_capacity: 32,
        control_channel_full_count: 2,
        release_control_send_fail_count: 1,
        flush_control_send_fail_count: 1,
    };
    fake_decoder.set_control_pressure(pressure);
    pipeline.set_video_decoder_thread(fake_decoder);

    assert_eq!(
        pipeline.video_decoder_control_channel_pressure(),
        Some(pressure)
    );
}

#[test]
fn playback_pipeline_decoder_boundary_propagates_flush_error() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    fake_decoder.fail_flush_with("flush failed at decoder boundary");
    pipeline.set_video_decoder_thread(fake_decoder);

    let flush_error = pipeline
        .flush_video_decoder_thread()
        .expect_err("fake decoder flush should fail");

    assert_eq!(flush_error.to_string(), "flush failed at decoder boundary");
}

#[test]
fn playback_pipeline_decoder_boundary_releases_active_fake_frame() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let released_decoder = fake_decoder.clone();
    let resource_handle = video_core::FrameResourceHandle(42);
    pipeline.set_video_decoder_thread(fake_decoder);

    assert!(pipeline.can_receive_decoded_video_frames());
    assert!(pipeline.release_frame_to_video_decoder(resource_handle));
    assert_eq!(released_decoder.released_handles(), vec![resource_handle]);
}

#[test]
fn snapshot_backend_name_uses_pipeline_decoder_boundary() {
    let mut session = PlayerSession::new();

    session
        .pipeline
        .set_video_decoder_thread(SharedFakeVideoDecoderThread::new());

    let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());

    assert_eq!(
        snapshot.active_backend.name.as_deref(),
        Some("Shared fake decoder")
    );
}

#[test]
fn clear_queued_video_frames_releases_queue_and_fallback_without_present() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(50), 3));
    session
        .replace_seek_preroll_fallback_frame(decoded_frame_for_tests(Duration::from_millis(40), 4));

    session.clear_queued_video_frames();

    let released_handles = fake_decoder.released_handles();
    assert_eq!(released_handles.len(), 3);
    assert!(released_handles.contains(&video_core::FrameResourceHandle(1)));
    assert!(released_handles.contains(&video_core::FrameResourceHandle(2)));
    assert!(released_handles.contains(&video_core::FrameResourceHandle(4)));
    assert!(
        !released_handles.contains(&video_core::FrameResourceHandle(3)),
        "present frame ownership must stay explicit"
    );
    assert!(session.pipeline.video_present_queue_is_empty());
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(3))
    );
    assert!(!session.pipeline.has_seek_preroll_fallback_video_frame());
}
