use super::*;
use codec_core::VideoColorMetadata;
use video_core::FrameResourceHandle;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

/// Создаёт decoded frame без реальных GPU resources для channel-level тестов.
fn decoded_frame_for_tests(handle_id: u64) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(handle_id),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

/// Проверяет, что subscription увидела новый activity epoch после observed marker-а.
fn assert_activity_received_after(
    subscription: &VideoDecoderActivitySubscription,
    observed_epoch: video_core::VideoDecoderActivityEpoch,
) -> video_core::VideoDecoderActivityEpoch {
    match subscription.activity_since(observed_epoch) {
        video_core::VideoDecoderActivityWaitOutcome::ActivityReceived { epoch } => epoch,
        other => panic!("activity epoch должен продвинуться, got {other:?}"),
    }
}

/// Проверяет, что повторная запись того же state не создаёт ложную activity.
fn assert_no_activity_after(
    subscription: &VideoDecoderActivitySubscription,
    observed_epoch: video_core::VideoDecoderActivityEpoch,
) {
    match subscription.activity_since(observed_epoch) {
        video_core::VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch { .. } => {}
        other => panic!("activity epoch не должен был продвинуться, got {other:?}"),
    }
}

/// Проверяет, что public error contract сохраняет причину fatal остановки thread-а.
#[test]
fn decode_thread_error_exposes_message_for_player_layer() {
    let error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

    assert_eq!(error.message(), "P010 DMA-BUF zero-copy import failed");
    assert_eq!(error.to_string(), "P010 DMA-BUF zero-copy import failed");
}

/// Проверяет parsing policy без изменения process env в параллельных тестах.
#[test]
fn flush_timeout_config_rejects_zero_and_non_numeric_values() {
    assert!(VideoDecodeThreadConfig::parse_flush_timeout("0").is_err());
    assert!(VideoDecodeThreadConfig::parse_flush_timeout("abc").is_err());
    assert_eq!(
        VideoDecodeThreadConfig::parse_flush_timeout("25").unwrap(),
        Duration::from_millis(25)
    );
}

/// Проверяет, что direct API caller не может случайно создать unbounded/zero queues.
#[test]
fn decoder_thread_config_normalizes_zero_queue_limits() {
    let config = VideoDecodeThreadConfig {
        packet_channel_frames: 0,
        frame_channel_frames: 0,
        control_channel_frames: 0,
        decoder_ready_queue_frames: 0,
        decoder_surface_pool_frames: 0,
        zero_copy_surface_pool_slots: 0,
        flush_timeout: Duration::ZERO,
    }
    .normalized();

    assert_eq!(config.packet_channel_frames, 1);
    assert_eq!(config.frame_channel_frames, 1);
    assert_eq!(config.control_channel_frames, 1);
    assert_eq!(config.decoder_ready_queue_frames, 1);
    assert_eq!(config.decoder_surface_pool_frames, 1);
    assert_eq!(config.zero_copy_surface_pool_slots, 1);
    assert_eq!(config.flush_timeout, Duration::from_millis(1));
}

/// Проверяет, что texture view lookup считает lock wait даже при missing views.
#[test]
fn resource_lookup_reports_lock_wait_without_changing_missing_views_semantics() {
    let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());
    let lock_started_at = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("test start instant should allow small subtraction");

    let lookup = resource_lookup_from_pool_started_at(
        &resource_pool,
        FrameResourceHandle(99),
        lock_started_at,
    );

    match lookup {
        VideoFrameResourceLookup::Missing { lock_diagnostics } => {
            assert!(lock_diagnostics.wait >= Duration::from_millis(1));
        }
        VideoFrameResourceLookup::Ready { .. }
        | VideoFrameResourceLookup::Busy { .. }
        | VideoFrameResourceLookup::Fatal { .. } => {
            panic!("missing handle should keep missing semantics");
        }
    }
}

/// Проверяет, что non-blocking lookup возвращает Busy, пока resource pool lock удержан.
#[test]
fn try_resource_lookup_reports_busy_when_resource_pool_lock_is_held() {
    let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());
    let _held_resource_pool_lock = resource_pool
        .lock()
        .expect("test mutex should lock before try lookup");
    let lock_started_at = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("test start instant should allow small subtraction");

    let lookup = try_resource_lookup_from_pool_started_at(
        &resource_pool,
        FrameResourceHandle(99),
        lock_started_at,
    );

    match lookup {
        VideoFrameResourceLookup::Busy { lock_diagnostics } => {
            assert!(lock_diagnostics.wait >= Duration::from_millis(1));
        }
        VideoFrameResourceLookup::Ready { .. }
        | VideoFrameResourceLookup::Missing { .. }
        | VideoFrameResourceLookup::Fatal { .. } => {
            panic!("held mutex should produce busy without get_views");
        }
    }
}

/// Проверяет, что non-blocking Missing остаётся отличимым от Busy.
#[test]
fn try_resource_lookup_keeps_missing_distinct_from_busy() {
    let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());

    let lookup = try_resource_lookup_from_pool(&resource_pool, FrameResourceHandle(123));

    match lookup {
        VideoFrameResourceLookup::Missing { .. } => {}
        VideoFrameResourceLookup::Ready { .. }
        | VideoFrameResourceLookup::Busy { .. }
        | VideoFrameResourceLookup::Fatal { .. } => {
            panic!("available pool with unknown handle should be missing");
        }
    }
}

/// Проверяет, что poisoned mutex остаётся ошибочным состоянием, а не Busy/Missing.
#[test]
fn try_resource_lookup_reports_fatal_when_resource_pool_mutex_is_poisoned() {
    let resource_pool = Arc::new(Mutex::new(crate::resource_pool::FrameResourcePool::new()));
    let poison_resource_pool = Arc::clone(&resource_pool);
    let _ = std::thread::spawn(move || {
        let _held_resource_pool_lock = poison_resource_pool
            .lock()
            .expect("test mutex should lock before poisoning");
        panic!("poison resource pool mutex for lookup test");
    })
    .join();

    let lookup = try_resource_lookup_from_pool(resource_pool.as_ref(), FrameResourceHandle(123));

    match lookup {
        VideoFrameResourceLookup::Fatal { .. } => {}
        VideoFrameResourceLookup::Ready { .. }
        | VideoFrameResourceLookup::Busy { .. }
        | VideoFrameResourceLookup::Missing { .. } => {
            panic!("poisoned mutex should preserve error semantics");
        }
    }
}

/// Проверяет bounded decoded-frame publish: full channel не дропает frame молча.
#[test]
fn frame_publish_keeps_pending_frame_when_channel_is_full() {
    let (frame_tx, frame_rx) = bounded(1);
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(4);
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = activity_subscription.current_epoch();
    let mut publish_pressure = FramePublishPressureCounters::default();
    frame_tx
        .try_send(decoded_frame_for_tests(1))
        .expect("test channel has one free slot");
    let mut pending_publish = Some(PendingFramePublish::new(decoded_frame_for_tests(2)));

    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    let retry_observed_epoch =
        assert_activity_received_after(&activity_subscription, observed_epoch);
    assert!(pending_publish.is_some());
    let first_pressure = match diagnostic_rx
        .try_recv()
        .expect("full frame channel should emit pressure diagnostics")
    {
        VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => pressure,
        VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed { .. } => {
            panic!("publish pressure test should not emit preroll suppression diagnostics")
        }
        VideoDecoderDiagnosticEvent::FrameDropped { .. } => {
            panic!("publish pressure test should not emit frame drop diagnostics")
        }
    };
    assert_eq!(first_pressure.frame_publish_channel_full_count, 1);
    assert_eq!(first_pressure.pending_publish_retry_count, 0);
    assert_eq!(
        frame_rx.try_recv().unwrap().resource_handle,
        FrameResourceHandle(1)
    );

    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    assert_activity_received_after(&activity_subscription, retry_observed_epoch);
    assert!(pending_publish.is_none());
    let retry_pressure = match diagnostic_rx
        .try_recv()
        .expect("successful retry should emit retry diagnostics")
    {
        VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => pressure,
        VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed { .. } => {
            panic!("publish retry test should not emit preroll suppression diagnostics")
        }
        VideoDecoderDiagnosticEvent::FrameDropped { .. } => {
            panic!("publish retry test should not emit frame drop diagnostics")
        }
    };
    assert_eq!(retry_pressure.frame_publish_channel_full_count, 1);
    assert_eq!(retry_pressure.pending_publish_retry_count, 1);
    let published_frame = frame_rx.try_recv().unwrap();
    assert_eq!(published_frame.resource_handle, FrameResourceHandle(2));
    assert!(
        published_frame
            .diagnostics
            .timings
            .decoded_frame_publish_latency
            .is_some()
    );
    assert_eq!(
        retry_pressure.max_decoded_frame_publish_latency,
        retry_pressure.total_decoded_frame_publish_latency
    );
}

/// Проверяет, что diagnostics/error paths будят neutral decoder activity wait.
#[test]
fn activity_epoch_advances_on_diagnostic_and_error_events() {
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(1);
    let diagnostic_observed_epoch = activity_subscription.current_epoch();

    send_frame_publish_pressure_event(
        &diagnostic_tx,
        VideoFramePublishPressureDiagnostics::default(),
        &activity_notifier,
    );
    let error_observed_epoch =
        assert_activity_received_after(&activity_subscription, diagnostic_observed_epoch);
    assert!(diagnostic_rx.try_recv().is_ok());

    let (error_tx, error_rx) = bounded(1);
    send_decoder_thread_error(
        &error_tx,
        "fatal decode path for activity test".to_owned(),
        &activity_notifier,
    );

    assert_activity_received_after(&activity_subscription, error_observed_epoch);
    assert_eq!(
        error_rx.try_recv().unwrap().message(),
        "fatal decode path for activity test"
    );
}

/// Проверяет, что control-channel pressure counters различают release и flush failures.
#[test]
fn control_channel_pressure_counts_full_failures_by_operation() {
    let (control_tx, _control_rx) = bounded(1);
    let pressure_counters = DecoderControlChannelPressureCounters::default();
    if control_tx
        .try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(1)))
        .is_err()
    {
        panic!("test control channel has one free slot before pressure setup");
    }

    let release_error =
        match control_tx.try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(2))) {
            Ok(()) => panic!("full control channel must reject release message"),
            Err(error) => error,
        };
    let release_message = record_decoder_control_send_failure(
        DecoderControlOperation::Release,
        &control_tx,
        &pressure_counters,
        &release_error,
    );

    assert!(release_message.contains("zero-copy release"));
    let after_release = pressure_counters.snapshot(&control_tx);
    assert_eq!(after_release.control_channel_len, 1);
    assert_eq!(after_release.control_channel_capacity, 1);
    assert_eq!(after_release.control_channel_full_count, 1);
    assert_eq!(after_release.release_control_send_fail_count, 1);
    assert_eq!(after_release.flush_control_send_fail_count, 0);

    let (done_tx, _done_rx) = bounded(1);
    let flush_error = match control_tx.try_send(ThreadControlMsg::Flush(done_tx)) {
        Ok(()) => panic!("full control channel must reject flush message"),
        Err(error) => error,
    };
    let flush_message = record_decoder_control_send_failure(
        DecoderControlOperation::Flush,
        &control_tx,
        &pressure_counters,
        &flush_error,
    );

    assert!(flush_message.contains("decoder flush"));
    let after_flush = pressure_counters.snapshot(&control_tx);
    assert_eq!(after_flush.control_channel_len, 1);
    assert_eq!(after_flush.control_channel_capacity, 1);
    assert_eq!(after_flush.control_channel_full_count, 2);
    assert_eq!(after_flush.release_control_send_fail_count, 1);
    assert_eq!(after_flush.flush_control_send_fail_count, 1);
}

/// Создаёт handle без фонового VA thread-а для sender-side control tests.
fn decoder_thread_for_control_tests(
    control_tx: Sender<ThreadControlMsg>,
    control_pressure: Arc<DecoderControlChannelPressureCounters>,
    thread_state: DecoderThreadState,
) -> VideoDecodeThread {
    let (packet_tx, _packet_rx) = bounded(1);
    let (_frame_tx, frame_rx) = bounded(1);
    let (_packet_ack_tx, packet_ack_rx) = unbounded();
    let (_error_tx, error_rx) = bounded(1);
    let (_diagnostic_tx, diagnostic_rx) =
        std::sync::mpsc::sync_channel(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
    let (_activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();

    VideoDecodeThread {
        packet_tx,
        control_tx,
        control_pressure,
        frame_rx,
        packet_ack_rx,
        error_rx,
        diagnostic_rx,
        activity_subscription,
        resource_pool: Arc::new(Mutex::new(crate::resource_pool::FrameResourcePool::new())),
        thread_state,
        stream_config: Arc::new(Mutex::new(None)),
        end_of_stream_drain_state: Arc::new(Mutex::new(
            video_core::VideoDecoderEndOfStreamDrainState::Idle,
        )),
        config: VideoDecodeThreadConfig {
            flush_timeout: Duration::from_millis(1),
            ..VideoDecodeThreadConfig::default()
        }
        .normalized(),
        backend_name: "VA-API test",
    }
}

/// Проверяет, что concrete VAAPI thread отдаёт neutral activity snapshot.
#[test]
fn decoder_thread_activity_snapshot_is_available() {
    let (control_tx, _control_rx) = bounded(1);
    let decoder_thread = decoder_thread_for_control_tests(
        control_tx,
        Arc::new(DecoderControlChannelPressureCounters::default()),
        DecoderThreadState::new(),
    );

    match decoder_thread.decoder_activity_snapshot() {
        VideoDecoderActivitySnapshot::Available { captured_epoch, .. } => {
            assert_eq!(
                captured_epoch,
                video_core::VideoDecoderActivityEpoch::INITIAL
            );
        }
        VideoDecoderActivitySnapshot::Unavailable { reason } => {
            panic!("VAAPI activity snapshot должен быть Available, got {reason:?}");
        }
    }
}

/// Проверяет, что EOF drain state changes продвигают neutral activity epoch.
#[test]
fn activity_epoch_advances_on_eof_drain_state_changes() {
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let end_of_stream_drain_state = Arc::new(Mutex::new(
        video_core::VideoDecoderEndOfStreamDrainState::Idle,
    ));
    let draining_observed_epoch = activity_subscription.current_epoch();

    set_decoder_eof_drain_state(
        &end_of_stream_drain_state,
        video_core::VideoDecoderEndOfStreamDrainState::Draining { generation: 41 },
        &activity_notifier,
    )
    .expect("test EOF state mutex should be writable");
    let drained_observed_epoch =
        assert_activity_received_after(&activity_subscription, draining_observed_epoch);

    set_decoder_eof_drain_state(
        &end_of_stream_drain_state,
        video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 41 },
        &activity_notifier,
    )
    .expect("test EOF state mutex should be writable");
    let fatal_observed_epoch =
        assert_activity_received_after(&activity_subscription, drained_observed_epoch);

    let fatal_state = video_core::VideoDecoderEndOfStreamDrainState::Fatal {
        generation: Some(41),
        error: DecodeThreadError::new("EOF drain fatal for activity test").into(),
    };
    set_decoder_eof_drain_state(
        &end_of_stream_drain_state,
        fatal_state.clone(),
        &activity_notifier,
    )
    .expect("test EOF state mutex should be writable");
    let unchanged_observed_epoch =
        assert_activity_received_after(&activity_subscription, fatal_observed_epoch);

    set_decoder_eof_drain_state(&end_of_stream_drain_state, fatal_state, &activity_notifier)
        .expect("test EOF state mutex should be writable");
    assert_no_activity_after(&activity_subscription, unchanged_observed_epoch);
}

/// Проверяет, что set floor на full control channel возвращает typed Backpressure.
#[test]
fn preroll_output_floor_set_control_channel_full_returns_backpressure() {
    let (control_tx, _control_rx) = bounded(1);
    control_tx
        .try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(1)))
        .expect("test control channel has one slot");
    let control_pressure = Arc::new(DecoderControlChannelPressureCounters::default());
    let decoder_thread = decoder_thread_for_control_tests(
        control_tx,
        Arc::clone(&control_pressure),
        DecoderThreadState::new(),
    );

    let result = decoder_thread.set_preroll_output_floor(video_core::VideoPrerollOutputFloor {
        generation: 7,
        floor_pts: Duration::from_millis(1_000),
        retain_latest_before_floor: true,
    });

    match result {
        video_core::VideoPrerollOutputFloorResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages,
                capacity,
            },
        ) => {
            assert_eq!(queued_messages, 1);
            assert_eq!(capacity, 1);
        }
        other => panic!("full control channel must return Backpressure, got {other:?}"),
    }
    let pressure = decoder_thread.control_channel_pressure_stats();
    assert_eq!(pressure.control_channel_full_count, 1);
    assert_eq!(pressure.release_control_send_fail_count, 0);
    assert_eq!(pressure.flush_control_send_fail_count, 0);
}

/// Проверяет, что clear floor использует тот же typed Backpressure reason.
#[test]
fn preroll_output_floor_clear_control_channel_full_returns_backpressure() {
    let (control_tx, _control_rx) = bounded(1);
    control_tx
        .try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(1)))
        .expect("test control channel has one slot");
    let decoder_thread = decoder_thread_for_control_tests(
        control_tx,
        Arc::new(DecoderControlChannelPressureCounters::default()),
        DecoderThreadState::new(),
    );

    let result =
        decoder_thread.clear_preroll_output_floor(video_core::VideoPrerollOutputFloorClear::Any);

    assert!(matches!(
        result,
        video_core::VideoPrerollOutputFloorResult::Backpressure(
            video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 1,
                capacity: 1,
            },
        )
    ));
}

/// Проверяет fail-closed path: sticky fatal thread state не отправляет новую floor command.
#[test]
fn preroll_output_floor_control_returns_fatal_when_thread_is_fatal() {
    let (control_tx, _control_rx) = bounded(1);
    let thread_state = DecoderThreadState::new();
    thread_state.mark_fatal(DecodeThreadError::new("test decoder fatal"));
    let decoder_thread = decoder_thread_for_control_tests(
        control_tx,
        Arc::new(DecoderControlChannelPressureCounters::default()),
        thread_state,
    );

    let result =
        decoder_thread.clear_preroll_output_floor(video_core::VideoPrerollOutputFloorClear::Any);

    match result {
        video_core::VideoPrerollOutputFloorResult::Fatal(error) => {
            assert_eq!(error.message(), "test decoder fatal");
        }
        other => panic!("fatal decoder thread must return Fatal, got {other:?}"),
    }
}

/// Проверяет timeout ACK для floor command как fatal lifecycle error.
#[test]
fn preroll_output_floor_ack_timeout_marks_thread_fatal_once() {
    let (_done_tx, done_rx) = bounded(1);
    let thread_state = DecoderThreadState::new();

    let result = wait_for_preroll_output_floor_ack(
        done_rx,
        Duration::from_millis(1),
        &thread_state,
        "preroll output-floor set",
    );

    match result {
        video_core::VideoPrerollOutputFloorResult::Fatal(error) => {
            assert!(
                error
                    .to_string()
                    .contains("Decoder thread did not confirm preroll output-floor set within")
            );
        }
        other => panic!("empty preroll floor ACK channel must timeout, got {other:?}"),
    }
    assert!(thread_state.current_error().is_some());
    assert!(thread_state.take_pending_error().is_some());
    assert!(thread_state.take_pending_error().is_none());
}

/// Проверяет seek/flush cancellation: старые packets не остаются после backend flush.
#[test]
fn flush_drops_queued_decode_packets() {
    let (packet_tx, packet_rx) = bounded(4);
    for packet_index in 0..3u64 {
        packet_tx
            .try_send(QueuedDecodePacket {
                packet: DecodePacket {
                    track_id: media_core::TrackId::new(1),
                    pts: Duration::from_millis(packet_index),
                    dts: None,
                    track_dts: None,
                    generation: packet_index,
                    encoded_bytes: Bytes::from_static(b"vp9"),
                    keyframe: packet_index == 0,
                    resolved_color: None,
                },
                enqueued_at: Instant::now(),
            })
            .expect("test packet channel has capacity");
    }

    assert_eq!(drain_queued_decode_packets(&packet_rx), 3);
    assert!(matches!(packet_rx.try_recv(), Err(TryRecvError::Empty)));
}

/// Проверяет packet-based ACK: отсутствие output frame не блокирует accounting.
#[test]
fn accepted_none_decode_outcome_acks_packet_without_publish() {
    let (frame_tx, frame_rx) = bounded(1);
    let (packet_ack_tx, packet_ack_rx) = bounded(1);
    let (error_tx, error_rx) = bounded(1);
    let (diagnostic_tx, diagnostic_rx) =
        std::sync::mpsc::sync_channel(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = activity_subscription.current_epoch();
    let mut pending_publish = None;
    let mut publish_pressure = FramePublishPressureCounters::default();
    let mut latest_color_metadata = None;
    let queued_packet = QueuedDecodePacket {
        packet: DecodePacket {
            track_id: media_core::TrackId::new(1),
            pts: Duration::from_millis(900),
            dts: None,
            track_dts: None,
            generation: 17,
            encoded_bytes: Bytes::from_static(b"suppressed"),
            keyframe: false,
            resolved_color: Some(VideoColorMetadata::sdr_bt709_limited()),
        },
        enqueued_at: Instant::now(),
    };

    let result = handle_decode_packet_outcome(
        Ok(VaapiDecodePacketOutcome::Accepted(None)),
        queued_packet,
        Duration::from_millis(2),
        DecodeQueuedPacketContext {
            frame_tx: &frame_tx,
            packet_ack_tx: &packet_ack_tx,
            error_tx: &error_tx,
            pending_publish: &mut pending_publish,
            publish_pressure: &mut publish_pressure,
            diagnostic_tx: &diagnostic_tx,
            activity_notifier: &activity_notifier,
            latest_color_metadata: &mut latest_color_metadata,
        },
    );

    assert!(matches!(result, DecodeQueuedPacketResult::Continue));
    assert!(packet_ack_rx.try_recv().is_ok());
    assert_activity_received_after(&activity_subscription, observed_epoch);
    assert!(matches!(frame_rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(error_rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(diagnostic_rx.try_recv().is_err());
    assert!(pending_publish.is_none());
    assert_eq!(
        latest_color_metadata,
        Some(VideoColorMetadata::sdr_bt709_limited())
    );
}

/// Проверяет, что dropped activity receiver не превращается в fatal decode error.
#[test]
fn disconnected_activity_receiver_does_not_create_fatal_decode_error() {
    let (frame_tx, _frame_rx) = bounded(1);
    let (packet_ack_tx, packet_ack_rx) = bounded(1);
    let (error_tx, error_rx) = bounded(1);
    let (diagnostic_tx, diagnostic_rx) =
        std::sync::mpsc::sync_channel(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    drop(activity_subscription);
    let mut pending_publish = None;
    let mut publish_pressure = FramePublishPressureCounters::default();
    let mut latest_color_metadata = None;
    let queued_packet = QueuedDecodePacket {
        packet: DecodePacket {
            track_id: media_core::TrackId::new(1),
            pts: Duration::from_millis(901),
            dts: None,
            track_dts: None,
            generation: 18,
            encoded_bytes: Bytes::from_static(b"disconnected-activity"),
            keyframe: false,
            resolved_color: None,
        },
        enqueued_at: Instant::now(),
    };

    let result = handle_decode_packet_outcome(
        Ok(VaapiDecodePacketOutcome::Accepted(None)),
        queued_packet,
        Duration::from_millis(1),
        DecodeQueuedPacketContext {
            frame_tx: &frame_tx,
            packet_ack_tx: &packet_ack_tx,
            error_tx: &error_tx,
            pending_publish: &mut pending_publish,
            publish_pressure: &mut publish_pressure,
            diagnostic_tx: &diagnostic_tx,
            activity_notifier: &activity_notifier,
            latest_color_metadata: &mut latest_color_metadata,
        },
    );

    assert!(matches!(result, DecodeQueuedPacketResult::Continue));
    assert!(packet_ack_rx.try_recv().is_ok());
    assert!(matches!(error_rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(diagnostic_rx.try_recv().is_err());
    assert!(pending_publish.is_none());
}

/// Проверяет, что timeout не блокируется бесконечно и становится fatal state.
#[test]
fn flush_ack_timeout_marks_thread_fatal_once() {
    let (_done_tx, done_rx) = bounded(1);
    let thread_state = DecoderThreadState::new();

    let error = wait_for_flush_ack(done_rx, Duration::from_millis(1), &thread_state)
        .expect_err("empty ACK channel must timeout");

    assert!(
        error
            .to_string()
            .contains("Decoder thread did not confirm flush within")
    );
    assert!(thread_state.current_error().is_some());
    assert!(thread_state.take_pending_error().is_some());
    assert!(thread_state.take_pending_error().is_none());
}

/// Проверяет, что player не видит EOF drain завершённым, пока frame channel несёт tail.
#[test]
fn eof_drain_state_stays_draining_while_decoded_tail_waits_in_channel() {
    let visible_state = player_visible_eof_drain_state(
        video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 7 },
        1,
    );

    assert_eq!(
        visible_state,
        video_core::VideoDecoderEndOfStreamDrainState::Draining { generation: 7 }
    );
    assert_eq!(
        player_visible_eof_drain_state(
            video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 7 },
            0,
        ),
        video_core::VideoDecoderEndOfStreamDrainState::Drained { generation: 7 }
    );
}

/// Проверяет timeout explicit EOF drain отдельно от seek flush timeout-а.
#[test]
fn eof_drain_ack_timeout_marks_thread_fatal_once() {
    let (_done_tx, done_rx) = bounded(1);
    let thread_state = DecoderThreadState::new();

    let result = wait_for_end_of_stream_drain_ack(done_rx, Duration::from_millis(1), &thread_state);

    match result {
        video_core::VideoDecoderEndOfStreamDrainResult::Fatal(error) => {
            assert!(
                error
                    .to_string()
                    .contains("Decoder thread did not confirm EOF drain within")
            );
        }
        other => panic!("empty EOF drain ACK channel must timeout, got {other:?}"),
    }
    assert!(thread_state.current_error().is_some());
    assert!(thread_state.take_pending_error().is_some());
    assert!(thread_state.take_pending_error().is_none());
}
