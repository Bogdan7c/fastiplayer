use std::{num::NonZeroUsize, time::Duration};

use super::*;

#[derive(Clone)]
struct TestResourceProvider;

struct DefaultPrerollFloorDecoderThread;

impl VideoDecoderThreadHandle for DefaultPrerollFloorDecoderThread {
    type ResourceProvider = TestResourceProvider;

    fn backend_name(&self) -> &'static str {
        "default-preroll-floor-test"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        Ok(())
    }

    fn release_frame(&self, _handle: crate::FrameResourceHandle) {}

    fn try_recv_frame(&self) -> Option<crate::DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<crate::VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        TestResourceProvider
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        None
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

/// Проверяет default set behavior: unsupported backend не маскируется под no-op.
#[test]
fn default_preroll_output_floor_set_returns_unsupported() {
    let decoder_thread = DefaultPrerollFloorDecoderThread;
    let floor = VideoPrerollOutputFloor {
        generation: 7,
        floor_pts: Duration::from_millis(1_500),
        retain_latest_before_floor: true,
    };

    assert_eq!(
        decoder_thread.set_preroll_output_floor(floor),
        VideoPrerollOutputFloorResult::Unsupported
    );
}

/// Проверяет default clear behavior: отсутствие support остаётся harmless no-op.
#[test]
fn default_preroll_output_floor_clear_returns_unchanged() {
    let decoder_thread = DefaultPrerollFloorDecoderThread;

    assert_eq!(
        decoder_thread
            .clear_preroll_output_floor(VideoPrerollOutputFloorClear::MatchingGeneration(7)),
        VideoPrerollOutputFloorResult::Unchanged
    );
}

/// Проверяет default activity contract: старый backend получает typed unsupported fallback.
#[test]
fn default_decoder_activity_snapshot_returns_unsupported() {
    let decoder_thread = DefaultPrerollFloorDecoderThread;

    match decoder_thread.decoder_activity_snapshot() {
        VideoDecoderActivitySnapshot::Unavailable { reason } => {
            assert_eq!(
                reason,
                VideoDecoderActivityUnavailableReason::UnsupportedNotifier
            );
        }
        VideoDecoderActivitySnapshot::Available { .. } => {
            panic!("default decoder handle must not expose activity subscription");
        }
    }
}

/// Проверяет default software-resource contract: hardware/old backend не маскируется под нули.
#[test]
fn default_host_upload_resource_snapshot_returns_unsupported() {
    let decoder_thread = DefaultPrerollFloorDecoderThread;

    assert_eq!(
        decoder_thread.host_upload_resource_snapshot(),
        HostUploadResourceSnapshotStatus::UnsupportedBackend
    );
}

/// Проверяет happy path: caller видит activity после ранее observed epoch-а.
#[test]
fn decoder_activity_wait_receives_activity_after_observed_epoch() {
    let (notifier, subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = subscription.current_epoch();

    let notified_epoch = notifier.notify_activity();
    let outcome = subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

    assert_eq!(
        outcome,
        VideoDecoderActivityWaitOutcome::ActivityReceived {
            epoch: notified_epoch
        }
    );
}

/// Проверяет fallback timeout: без нового epoch-а outcome не маскируется под activity.
#[test]
fn decoder_activity_wait_times_out_without_new_epoch() {
    let (_notifier, subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = subscription.current_epoch();

    let outcome = subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

    assert_eq!(
        outcome,
        VideoDecoderActivityWaitOutcome::Timeout {
            observed_epoch,
            current_epoch: observed_epoch
        }
    );
}

/// Проверяет stale pulse: wakeup есть, но activity уже была учтена caller-ом.
#[test]
fn decoder_activity_wait_reports_no_new_activity_after_epoch() {
    let (notifier, subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = notifier.notify_activity();

    let outcome = subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

    assert_eq!(
        outcome,
        VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
            observed_epoch,
            current_epoch: observed_epoch
        }
    );
}

/// Проверяет disconnect как typed state, чтобы wait side не входил в tight ready-loop.
#[test]
fn decoder_activity_wait_reports_disconnected_notifier() {
    let (notifier, subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = subscription.current_epoch();
    drop(notifier);

    let outcome = subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

    assert_eq!(
        outcome,
        VideoDecoderActivityWaitOutcome::Unavailable {
            reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier
        }
    );
}

/// Проверяет coalescing: bounded pulse не копит очередь, но epoch сохраняет все активности.
#[test]
fn decoder_activity_coalescing_keeps_latest_epoch() {
    let (notifier, subscription) = VideoDecoderActivityNotifier::new();
    let observed_epoch = subscription.current_epoch();

    let first_epoch = notifier.notify_activity();
    let second_epoch = notifier.notify_activity();
    let third_epoch = notifier.notify_activity();

    assert_eq!(first_epoch.get(), observed_epoch.get() + 1);
    assert_eq!(second_epoch.get(), observed_epoch.get() + 2);
    assert_eq!(third_epoch.get(), observed_epoch.get() + 3);
    assert_eq!(
        subscription.snapshot().activity_since(observed_epoch),
        VideoDecoderActivityWaitOutcome::ActivityReceived { epoch: third_epoch }
    );
    assert_eq!(
        subscription.wait_for_activity_after(third_epoch, Duration::from_millis(1)),
        VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
            observed_epoch: third_epoch,
            current_epoch: third_epoch
        }
    );
    assert_eq!(
        subscription.wait_for_activity_after(third_epoch, Duration::from_millis(1)),
        VideoDecoderActivityWaitOutcome::Timeout {
            observed_epoch: third_epoch,
            current_epoch: third_epoch
        }
    );
}

/// Фиксирует, что result enum сохраняет typed backpressure и fatal payloads.
#[test]
fn preroll_output_floor_result_preserves_backpressure_and_fatal_payloads() {
    let backpressure_reason = VideoDecoderControlBackpressureReason::ControlChannelFull {
        queued_messages: 3,
        capacity: 4,
    };
    let fatal_error = DecodeThreadError::new("floor command failed");

    assert_eq!(
        VideoPrerollOutputFloorResult::Backpressure(backpressure_reason),
        VideoPrerollOutputFloorResult::Backpressure(
            VideoDecoderControlBackpressureReason::ControlChannelFull {
                queued_messages: 3,
                capacity: 4,
            }
        )
    );
    assert_eq!(
        VideoPrerollOutputFloorResult::Fatal(fatal_error.clone()),
        VideoPrerollOutputFloorResult::Fatal(DecodeThreadError::new(fatal_error.message()))
    );
}

/// Проверяет, что direct API caller не может создать zero-capacity очереди.
#[test]
fn decoder_thread_config_normalizes_zero_limits() {
    let config = VideoDecoderThreadConfig {
        packet_channel_frames: 0,
        frame_channel_frames: 0,
        control_channel_frames: 0,
        decoder_ready_queue_frames: 0,
        decoder_surface_pool_frames: 0,
        software_frame_pool_frames: 0,
        software_decode_thread_budget: SoftwareDecodeThreadBudget::auto(),
        zero_copy_surface_pool_slots: 0,
        flush_timeout: Duration::ZERO,
    }
    .normalized();

    assert_eq!(config.packet_channel_frames, 1);
    assert_eq!(config.frame_channel_frames, 1);
    assert_eq!(config.software_frame_pool_frames, 1);
    assert_eq!(
        config.software_decode_thread_budget,
        SoftwareDecodeThreadBudget::auto()
    );
    assert_eq!(config.control_channel_frames, 1);
    assert_eq!(config.decoder_ready_queue_frames, 1);
    assert_eq!(config.decoder_surface_pool_frames, 1);
    assert_eq!(config.zero_copy_surface_pool_slots, 1);
    assert_eq!(config.flush_timeout, Duration::from_millis(1));
}

/// Fixed software thread budget хранит только positive value на уровне типа.
#[test]
fn software_decode_thread_budget_preserves_fixed_positive_value() {
    let thread_count = NonZeroUsize::new(3).expect("test value is positive");
    let budget = SoftwareDecodeThreadBudget::fixed(thread_count);

    assert_eq!(budget.fixed_thread_count(), Some(thread_count));
    assert_eq!(
        SoftwareDecodeThreadBudget::auto().fixed_thread_count(),
        None
    );
}

/// Проверяет parsing policy env timeout-а без изменения process env.
#[test]
fn decoder_thread_config_flush_timeout_parser_rejects_invalid_values() {
    assert!(VideoDecoderThreadConfig::parse_flush_timeout("0").is_err());
    assert!(VideoDecoderThreadConfig::parse_flush_timeout("abc").is_err());
    assert_eq!(
        VideoDecoderThreadConfig::parse_flush_timeout("25").unwrap(),
        Duration::from_millis(25)
    );
}

/// Проверяет, что public error contract сохраняет текст root cause.
#[test]
fn decode_thread_error_exposes_message_for_player_layer() {
    let error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

    assert_eq!(error.message(), "P010 DMA-BUF zero-copy import failed");
    assert_eq!(error.to_string(), "P010 DMA-BUF zero-copy import failed");
}

/// Проверяет accounting helper без underflow при переполненном resource pool.
#[test]
fn decoder_resource_snapshot_available_slots_saturates() {
    let snapshot = DecoderResourceSnapshot {
        capacity: 2,
        slots: 2,
        in_use: 3,
        free_surfaces: 0,
        waiting_gpu_completion: 0,
        waiting_decoder_reuse: 0,
        import_failures: 0,
        imports_created: 0,
        imports_reused: 0,
        imports_replaced: 0,
    };

    assert_eq!(snapshot.available_slots(), 0);
}

/// Проверяет, что software snapshot не использует VA-API surface slots.
#[test]
fn host_upload_resource_snapshot_stores_software_counters() {
    let snapshot = HostUploadResourceSnapshot {
        host_frames_ready: 2,
        host_frames_in_flight: 3,
        upload_slots_capacity: 4,
        upload_slots_free: 1,
        upload_failures: 5,
    };

    assert_eq!(snapshot.host_frames_ready, 2);
    assert_eq!(snapshot.host_frames_in_flight, 3);
    assert_eq!(snapshot.upload_slots_capacity, 4);
    assert_eq!(snapshot.upload_slots_free, 1);
    assert_eq!(snapshot.upload_failures, 5);
}

/// Проверяет, что ready-queue pressure и upload-slot pressure остаются разными типами.
#[test]
fn host_upload_backpressure_distinguishes_ready_queue_and_upload_slots() {
    let ready_queue_full = HostUploadResourceSnapshot {
        host_frames_ready: 4,
        host_frames_in_flight: 1,
        upload_slots_capacity: 2,
        upload_slots_free: 1,
        upload_failures: 0,
    };
    let upload_slots_exhausted = HostUploadResourceSnapshot {
        host_frames_ready: 1,
        host_frames_in_flight: 2,
        upload_slots_capacity: 2,
        upload_slots_free: 0,
        upload_failures: 0,
    };

    assert_eq!(
        ready_queue_full.backpressure_reason(4),
        Some(HostUploadBackpressureReason::ReadyQueueFull {
            host_frames_ready: 4,
            capacity: 4,
        })
    );
    assert_eq!(
        upload_slots_exhausted.backpressure_reason(4),
        Some(HostUploadBackpressureReason::UploadSlotsExhausted {
            host_frames_in_flight: 2,
            upload_slots_capacity: 2,
        })
    );
}
