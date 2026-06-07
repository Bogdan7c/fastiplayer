use super::test_support::*;
use super::*;

#[test]
fn playback_pipeline_decoder_boundary_absent_thread_is_noop() {
    let pipeline = PlaybackPipeline::default();
    let config = video_core::VideoStreamDecodeConfig::from_requirement(
        TrackId::new(1),
        &VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    let floor = video_core::VideoPrerollOutputFloor {
        generation: 7,
        floor_pts: Duration::from_millis(500),
        retain_latest_before_floor: true,
    };

    assert!(!pipeline.has_active_video_decoder());
    assert_eq!(pipeline.video_backend_name(), "Synthetic (test)");
    assert!(pipeline.flush_video_decoder_thread().is_ok());
    assert_eq!(
        pipeline.configure_video_decoder_stream(config),
        video_core::VideoStreamConfigResult::AbsentDecoder
    );
    assert_eq!(
        pipeline.clear_video_decoder_stream(),
        video_core::VideoStreamConfigResult::AbsentDecoder
    );
    assert_eq!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::AbsentDecoder
    );
    assert_eq!(
        pipeline.clear_video_decoder_preroll_output_floor(
            video_core::VideoPrerollOutputFloorClear::MatchingGeneration(7)
        ),
        video_core::VideoPrerollOutputFloorResult::AbsentDecoder
    );
    assert_eq!(
        pipeline.begin_video_decoder_end_of_stream_drain(0),
        video_core::VideoDecoderEndOfStreamDrainResult::AbsentDecoder
    );
    assert_eq!(
        pipeline.video_decoder_end_of_stream_drain_state(),
        video_core::VideoDecoderEndOfStreamDrainState::Idle
    );
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
fn playback_pipeline_decoder_boundary_forwards_preroll_output_floor_results() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let floor = video_core::VideoPrerollOutputFloor {
        generation: 3,
        floor_pts: Duration::from_millis(1_200),
        retain_latest_before_floor: true,
    };
    pipeline.set_video_decoder_thread(fake_decoder.clone());

    assert_eq!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::Applied
    );
    assert_eq!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::Unchanged
    );
    assert_eq!(fake_decoder.preroll_floor_sets(), vec![floor, floor]);
    assert_eq!(
        pipeline.clear_video_decoder_preroll_output_floor(
            video_core::VideoPrerollOutputFloorClear::MatchingGeneration(3)
        ),
        video_core::VideoPrerollOutputFloorResult::Cleared
    );
    assert_eq!(
        pipeline.clear_video_decoder_preroll_output_floor(
            video_core::VideoPrerollOutputFloorClear::MatchingGeneration(3)
        ),
        video_core::VideoPrerollOutputFloorResult::Unchanged
    );
    assert_eq!(
        fake_decoder.preroll_floor_clears(),
        vec![
            video_core::VideoPrerollOutputFloorClear::MatchingGeneration(3),
            video_core::VideoPrerollOutputFloorClear::MatchingGeneration(3),
        ]
    );
}

#[test]
fn playback_pipeline_decoder_boundary_preserves_preroll_output_floor_error_states() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let floor = video_core::VideoPrerollOutputFloor {
        generation: 5,
        floor_pts: Duration::from_millis(900),
        retain_latest_before_floor: true,
    };
    fake_decoder.push_preroll_floor_result(video_core::VideoPrerollOutputFloorResult::Unsupported);
    fake_decoder.push_preroll_floor_result(
        video_core::VideoPrerollOutputFloorResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 4,
                capacity: 4,
            },
        ),
    );
    fake_decoder.push_preroll_floor_result(video_core::VideoPrerollOutputFloorResult::Fatal(
        DecodeThreadError::new("floor fatal"),
    ));
    pipeline.set_video_decoder_thread(fake_decoder);

    assert_eq!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::Unsupported
    );
    assert!(matches!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 4,
                capacity: 4
            }
        )
    ));
    assert!(matches!(
        pipeline.set_video_decoder_preroll_output_floor(floor),
        video_core::VideoPrerollOutputFloorResult::Fatal(error) if error.message() == "floor fatal"
    ));
}

#[test]
fn playback_pipeline_decoder_boundary_forwards_stream_config_results() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let config = video_core::VideoStreamDecodeConfig::from_requirement(
        TrackId::new(7),
        &VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420),
    );
    pipeline.set_video_decoder_thread(fake_decoder.clone());

    assert_eq!(
        pipeline.configure_video_decoder_stream(config.clone()),
        video_core::VideoStreamConfigResult::Configured
    );
    assert_eq!(
        pipeline.configure_video_decoder_stream(config.clone()),
        video_core::VideoStreamConfigResult::Unchanged
    );
    assert_eq!(fake_decoder.configured_streams(), vec![config]);
    assert_eq!(
        pipeline.clear_video_decoder_stream(),
        video_core::VideoStreamConfigResult::Cleared
    );
    assert_eq!(
        pipeline.clear_video_decoder_stream(),
        video_core::VideoStreamConfigResult::Unchanged
    );
    assert_eq!(fake_decoder.clear_stream_count(), 2);
}

#[test]
fn playback_pipeline_decoder_boundary_preserves_config_error_states() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let config = video_core::VideoStreamDecodeConfig::from_requirement(
        TrackId::new(1),
        &VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    fake_decoder.push_configure_result(video_core::VideoStreamConfigResult::Unsupported(
        video_core::VideoStreamConfigRejection::UnsupportedCodec {
            codec: VideoCodec::H264,
        },
    ));
    fake_decoder.push_configure_result(video_core::VideoStreamConfigResult::Backpressure(
        video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: 4,
            capacity: 4,
        },
    ));
    fake_decoder.push_configure_result(video_core::VideoStreamConfigResult::Fatal(
        DecodeThreadError::new("configure fatal"),
    ));
    pipeline.set_video_decoder_thread(fake_decoder);

    assert!(matches!(
        pipeline.configure_video_decoder_stream(config.clone()),
        video_core::VideoStreamConfigResult::Unsupported(
            video_core::VideoStreamConfigRejection::UnsupportedCodec {
                codec: VideoCodec::H264
            }
        )
    ));
    assert!(matches!(
        pipeline.configure_video_decoder_stream(config.clone()),
        video_core::VideoStreamConfigResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 4,
                capacity: 4
            }
        )
    ));
    assert!(matches!(
        pipeline.configure_video_decoder_stream(config),
        video_core::VideoStreamConfigResult::Fatal(error) if error.message() == "configure fatal"
    ));
}

#[test]
fn playback_pipeline_decoder_boundary_forwards_eof_drain_without_seek_flush() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    pipeline.set_video_decoder_thread(fake_decoder.clone());

    assert_eq!(
        pipeline.begin_video_decoder_end_of_stream_drain(3),
        video_core::VideoDecoderEndOfStreamDrainResult::Started(
            video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 3 }
        )
    );
    assert_eq!(
        pipeline.begin_video_decoder_end_of_stream_drain(3),
        video_core::VideoDecoderEndOfStreamDrainResult::Unchanged(
            video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 3 }
        )
    );
    assert_eq!(
        pipeline.video_decoder_end_of_stream_drain_state(),
        video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 3 }
    );
    assert_eq!(fake_decoder.eof_drain_requests(), vec![3, 3]);
    assert_eq!(fake_decoder.flush_count(), 0);
}

#[test]
fn playback_pipeline_decoder_boundary_preserves_eof_drain_error_states() {
    let mut pipeline = PlaybackPipeline::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    fake_decoder.push_eof_drain_result(
        video_core::VideoDecoderEndOfStreamDrainResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 2,
                capacity: 2,
            },
        ),
    );
    fake_decoder.push_eof_drain_result(video_core::VideoDecoderEndOfStreamDrainResult::Fatal(
        DecodeThreadError::new("eof fatal"),
    ));
    pipeline.set_video_decoder_thread(fake_decoder);

    assert!(matches!(
        pipeline.begin_video_decoder_end_of_stream_drain(4),
        video_core::VideoDecoderEndOfStreamDrainResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 2,
                capacity: 2
            }
        )
    ));
    assert!(matches!(
        pipeline.begin_video_decoder_end_of_stream_drain(4),
        video_core::VideoDecoderEndOfStreamDrainResult::Fatal(error)
            if error.message() == "eof fatal"
    ));
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
