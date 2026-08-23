use std::time::Duration;

#[cfg(feature = "ffmpeg")]
use bytes::Bytes;
use codec_core::{
    ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
use video_core::VideoDecoderActivityWaitOutcome;

use super::test_support::*;
use super::*;

#[test]
fn start_decoder_thread_reports_feature_disabled_without_ffmpeg() {
    if cfg!(feature = "ffmpeg") {
        return;
    }

    let error = start_decoder_thread(FfmpegDecoderThreadConfig::default())
        .err()
        .expect("default build has no FFmpeg FFI");

    assert_eq!(error, FfmpegDecoderThreadError::FeatureDisabled);
}

/// Durable accumulator сохраняет burst больше прежней bounded ACK capacity.
#[test]
#[cfg(feature = "ffmpeg")]
fn packet_completion_counter_accumulates_and_drains_exactly_once() {
    // Seek fast-preroll допускает до 512 packets; ещё один completion доказывает
    // отсутствие скрытой зависимости от прежнего channel limit-а.
    const COMPLETION_BURST: usize = 513;

    // Test использует production accounting owner без codec/fixture side effects.
    let completion_counter = FfmpegPacketCompletionCounter::default();

    // Consumer намеренно не дренирует accumulator до завершения всего burst-а.
    for _ in 0..COMPLETION_BURST {
        completion_counter.record_completion();
    }

    // Первый drain обязан вернуть каждый completion из задержанного burst-а.
    assert_eq!(completion_counter.drain(), COMPLETION_BURST);

    // Второй drain закрепляет exactly-once transfer semantics.
    assert_eq!(completion_counter.drain(), 0);
}

/// AUD-015: disconnected frontend обязан завершать worker независимо от pool backpressure.
#[cfg(feature = "ffmpeg")]
#[test]
fn disconnected_frontend_terminates_worker_with_full_pool_and_queued_packet() {
    // Один слот позволяет детерминированно перевести production pool в состояние full.
    const HOST_POOL_CAPACITY: usize = 1;
    // Два packet slot-а гарантируют queued tail, даже если worker успеет принять первый packet.
    const PACKET_CHANNEL_CAPACITY: usize = 2;
    // Lifecycle shutdown не должен зависеть от освобождения удерживаемого frame resource.
    const TERMINATION_TIMEOUT: Duration = Duration::from_secs(1);

    // Запускаем настоящий frontend вместе с его production worker и lifecycle owner-ом.
    let thread_config = VideoDecoderThreadConfig {
        // Test-specific capacity переводит настоящий host pool в контролируемое состояние full.
        software_frame_pool_frames: HOST_POOL_CAPACITY,
        // Capacity два сохраняет хотя бы один queued packet при любой допустимой гонке старта.
        packet_channel_frames: PACKET_CHANNEL_CAPACITY,
        // Остальные neutral limits не участвуют в проверяемом lifecycle invariant.
        ..VideoDecoderThreadConfig::default()
    };
    let decoder = FfmpegVideoDecoderThread::spawn(FfmpegDecoderThreadConfig::from_thread_config(
        thread_config,
    ))
    .expect("production FFmpeg frontend and worker should start");

    // Clone удерживает provider для проверки pool и cleanup после frontend drop.
    let resource_provider = decoder.host_resource_provider.clone();
    // Реальный resource entry удерживает единственный pool slot до конца проверки.
    let held_resource = resource_provider
        .insert_frame(
            1,
            test_yuv420_frame(2, 2, 32),
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect("test AVFrame should fill the only host-pool slot");
    // Защищаем предусловие: worker обязан войти именно в full-pool control branch.
    assert_eq!(resource_provider.free_slots(), 0);

    // Два production send_packet вызова оставляют минимум один packet за full-pool gate.
    decoder
        .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("first packet should enter the production packet channel");
    decoder
        .send_packet(decode_packet_with_pts(1, 1, Duration::from_millis(1)))
        .expect("second packet should remain queued behind full-pool backpressure");
    // Проверяем exact defect precondition до lifecycle drop.
    assert!(decoder.packet_queue_depth() >= 1);

    // Hook наблюдает возврат настоящего Drop, внутри которого выполняется worker join.
    let (terminated_tx, terminated_rx) = bounded(1);
    // Отдельный thread позволяет ограничить ожидание синхронного production Drop.
    let frontend_drop_thread = std::thread::Builder::new()
        .name("aud-015-frontend-drop".to_owned())
        .spawn(move || {
            // Drop одновременно отключает frontend channels, сигналит shutdown и join-ит worker.
            drop(decoder);
            terminated_tx
                .send(())
                .expect("termination observer should remain connected");
        })
        .expect("AUD-015 frontend-drop observer thread should start");

    // Независимый shutdown обязан завершить worker, не требуя release held_resource.
    terminated_rx
        .recv_timeout(TERMINATION_TIMEOUT)
        .expect("worker did not terminate after frontend disconnect within bounded timeout");
    // Join observer-а закрепляет, что production Drop сам дождался worker lifecycle.
    frontend_drop_thread
        .join()
        .expect("bounded FFmpeg frontend drop should join cleanly");

    // Lease намеренно освобождается только после доказанного worker termination.
    resource_provider.release_frame(held_resource.handle);
}

#[test]
fn send_packet_retries_same_padded_packet_after_eagain() {
    let mut fake_api = ScriptedDecodeApi::default()
        .with_send_results([FakeSendResult::Again, FakeSendResult::Accepted])
        .with_receive_results([
            FakeReceiveResult::Frame(frame_timestamps(10, 11, 1)),
            FakeReceiveResult::Again,
            FakeReceiveResult::Again,
        ]);
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let mut decode_loop = SendReceiveDecodeLoop::new(
        fake_api.clone(),
        activity_notifier,
        shared_idle_drain_state(),
    );
    let observed_epoch = activity_subscription.current_epoch();
    let progress = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 100, Duration::from_millis(100)))
        .expect("EAGAIN drain should retry the same packet");

    fake_api = decode_loop.codec_api;

    assert_eq!(fake_api.created_packets.len(), 1);
    assert_eq!(fake_api.sent_packet_ids, vec![1, 1]);
    assert_eq!(fake_api.created_packets[0].payload(), &[1, 2, 3, 1]);
    assert!(
        fake_api.created_packets[0].padded_bytes()[4..]
            .iter()
            .all(|padding_byte| *padding_byte == 0)
    );
    assert!(progress.packet_completed);
    assert_eq!(progress.frames.len(), 1);
    assert_eq!(decode_loop.completed_packet_count, 1);
    assert!(matches!(
        activity_subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(0)),
        VideoDecoderActivityWaitOutcome::ActivityReceived { .. }
            | VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch { .. }
    ));
}

#[test]
fn receive_loop_allows_zero_one_or_many_frames_per_packet() {
    let mut zero_frame_loop = fake_loop([FakeSendResult::Accepted], [FakeReceiveResult::Again]);
    let zero_frame_progress = zero_frame_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("zero-frame packet should complete");
    assert!(zero_frame_progress.frames.is_empty());

    let mut one_frame_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Again,
        ],
    );
    let one_frame_progress = one_frame_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("one-frame packet should complete");
    assert_eq!(one_frame_progress.frames.len(), 1);

    let mut many_frame_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(2, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(3, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Again,
        ],
    );
    let many_frame_progress = many_frame_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("multi-frame packet should complete");
    assert_eq!(many_frame_progress.frames.len(), 3);
}

#[test]
fn receive_budget_caps_frames_and_marks_resource_budget_reached() {
    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(2, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(3, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Again,
        ],
    );

    let outcome = decode_loop
        .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO), 2)
        .expect("budgeted send should succeed");

    match outcome {
        SendPacketOutcome::Consumed(progress) => {
            // Pool fits only 2 frames; the third stays buffered inside FFmpeg.
            assert_eq!(progress.frames.len(), 2);
            assert!(progress.packet_completed);
            assert_eq!(
                progress.stop_reason,
                ReceiveStopReason::ResourceBudgetReached
            );
        }
        SendPacketOutcome::Deferred { .. } => {
            panic!("packet was accepted by FFmpeg; it must not be deferred")
        }
    }
}

#[test]
fn send_packet_defers_when_pool_budget_is_zero_and_ffmpeg_needs_drain() {
    let mut decode_loop = fake_loop([FakeSendResult::Again], []);

    let outcome = decode_loop
        .send_packet(decode_packet_with_pts(5, 0, Duration::from_millis(5)), 0)
        .expect("zero budget must defer instead of fatally overflowing the pool");

    match outcome {
        SendPacketOutcome::Deferred { progress, packet } => {
            assert!(progress.frames.is_empty());
            assert!(!progress.packet_completed);
            assert_eq!(packet.generation, 5);
        }
        SendPacketOutcome::Consumed(_) => {
            panic!("send returned EAGAIN with no pool budget; packet must be deferred")
        }
    }
}

#[test]
fn eof_drain_with_budget_reports_draining_when_capped() {
    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(2, NO_TIMESTAMP, 1)),
            FakeReceiveResult::EndOfFile,
        ],
    );

    let report = decode_loop
        .begin_end_of_stream_drain(9, 1)
        .expect("budgeted EOF drain should succeed");

    assert_eq!(report.frames.len(), 1);
    assert!(matches!(
        report.state,
        VideoDecoderEndOfStreamDrainState::Draining { generation: 9 }
    ));

    let continuation = decode_loop
        .begin_end_of_stream_drain(9, 1)
        .expect("released pool slot should continue receive-side EOF drain");

    assert_eq!(decode_loop.codec_api.end_of_stream_send_count, 1);
    assert_eq!(continuation.frames.len(), 1);
    assert_eq!(
        continuation.state,
        VideoDecoderEndOfStreamDrainState::Draining { generation: 9 }
    );

    let completion = decode_loop
        .begin_end_of_stream_drain(9, 1)
        .expect("next released slot should observe terminal FFmpeg EOF");

    assert_eq!(decode_loop.codec_api.end_of_stream_send_count, 1);
    assert!(completion.frames.is_empty());
    assert_eq!(
        completion.state,
        VideoDecoderEndOfStreamDrainState::Drained { generation: 9 }
    );
    assert_eq!(
        decode_loop.end_of_stream_drain_state(),
        VideoDecoderEndOfStreamDrainState::Drained { generation: 9 }
    );
}

#[test]
fn release_driven_eof_continuation_preserves_last_coalesced_release_edge() {
    // Первый bounded pass заполняет единственный свободный host-frame slot.
    // После presentation несколько освобождений могут слиться в один pulse;
    // worker всё равно обязан продолжить EOF owner loop, а не ждать packet-а.
    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Frame(frame_timestamps(2, NO_TIMESTAMP, 1)),
            FakeReceiveResult::EndOfFile,
        ],
    );

    let first_pass = decode_loop
        .begin_end_of_stream_drain(17, 1)
        .expect("первый bounded EOF pass должен отдать первый tail frame");
    let first_result = VideoDecoderEndOfStreamDrainResult::Started(first_pass.state.clone());
    assert!(
        eof_drain_result_requires_owner_reentry(&first_result),
        "Draining обязан вернуть owner loop к release-driven continuation"
    );

    // Один coalesced release edge открыл сразу два slots: один для последнего
    // tail frame, второй — чтобы тем же owner turn увидеть terminal EOF.
    let completion = decode_loop
        .begin_end_of_stream_drain(17, 2)
        .expect("последний release edge должен довести decoder до terminal EOF");
    let completion_result = VideoDecoderEndOfStreamDrainResult::Started(completion.state.clone());

    assert_eq!(completion.frames.len(), 1);
    assert_eq!(
        completion.state,
        VideoDecoderEndOfStreamDrainState::Drained { generation: 17 }
    );
    assert!(!eof_drain_result_requires_owner_reentry(&completion_result));
    assert_eq!(decode_loop.codec_api.end_of_stream_send_count, 1);
}

#[test]
fn accepted_ffmpeg_eof_rejects_receive_eagain_instead_of_spinning() {
    let mut decode_loop = fake_loop([FakeSendResult::Accepted], [FakeReceiveResult::Again]);

    let error = decode_loop
        .begin_end_of_stream_drain(23, 1)
        .expect_err("accepted FFmpeg EOF не допускает receive-side EAGAIN");

    assert!(matches!(
        error,
        FfmpegDecoderThreadError::ProtocolViolation { reason }
            if reason.contains("accepted EOF")
    ));
}

#[test]
fn receive_loop_uses_packet_color_when_frame_metadata_is_missing() {
    let expected_context_color = VideoColorMetadata::container(
        ColorRange::Full,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Hlg,
        None,
    );
    let mut packet = decode_packet_with_pts(1, 0, Duration::ZERO);
    packet.resolved_color = Some(expected_context_color.clone());
    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
            FakeReceiveResult::Again,
        ],
    );

    let progress = decode_loop
        .send_packet_for_test(packet)
        .expect("packet color should be copied into decoded frame record");

    assert_eq!(progress.frames.len(), 1);
    assert_eq!(progress.frames[0].color, Some(expected_context_color));
}

#[test]
fn frame_hdr_side_data_merges_with_packet_core_colorimetry() {
    let context_color = VideoColorMetadata::container(
        ColorRange::Limited,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        None,
    );
    let mut frame_side_data_color = VideoColorMetadata::bitstream(
        ColorRange::Unknown,
        MatrixCoefficients::Unknown,
        ColorPrimaries::Unknown,
        TransferFunction::Unknown,
    );
    frame_side_data_color.hdr_metadata = Some(HdrMetadata {
        color_primaries: ColorPrimaries::Unknown,
        transfer_function: TransferFunction::Unknown,
        max_luminance_nits: Some(1_000.0),
        min_luminance_nits: Some(0.005),
        max_content_light_level_nits: Some(1_000),
        max_frame_average_light_level_nits: Some(400),
    });

    let merged_color = merge_frame_color_with_context_color(
        Some(frame_side_data_color),
        &Some(context_color.clone()),
    )
    .expect("frame side data and packet color should merge");
    let hdr_metadata = merged_color
        .hdr_metadata
        .as_ref()
        .expect("HDR side data should be preserved");

    assert_eq!(merged_color.range, context_color.range);
    assert_eq!(merged_color.matrix, context_color.matrix);
    assert_eq!(merged_color.primaries, context_color.primaries);
    assert_eq!(merged_color.transfer, context_color.transfer);
    assert_eq!(merged_color.origin, context_color.origin);
    assert_eq!(merged_color.confidence, context_color.confidence);
    assert_eq!(hdr_metadata.color_primaries, ColorPrimaries::Bt2020);
    assert_eq!(hdr_metadata.transfer_function, TransferFunction::Pq);
    assert_eq!(hdr_metadata.max_luminance_nits, Some(1_000.0));
    assert!(merged_color.requires_hdr_processing());
}

#[test]
fn flush_and_eof_drain_have_distinct_lifecycle_effects() {
    let mut decode_loop = fake_loop([FakeSendResult::Accepted], [FakeReceiveResult::Again]);
    decode_loop
        .send_packet_for_test(decode_packet_with_pts(7, 5, Duration::from_millis(5)))
        .expect("packet should seed generation");

    assert_eq!(decode_loop.current_generation, Some(7));

    decode_loop
        .flush_for_seek()
        .expect("seek flush should clear state");

    assert_eq!(decode_loop.current_generation, None);
    assert_eq!(
        decode_loop.end_of_stream_drain_state(),
        VideoDecoderEndOfStreamDrainState::Idle
    );
    assert_eq!(decode_loop.codec_api.flush_buffers_count, 1);

    decode_loop
        .codec_api
        .push_send_results([FakeSendResult::Accepted]);
    decode_loop.codec_api.push_receive_results([
        FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, NO_TIMESTAMP, 4)),
        FakeReceiveResult::EndOfFile,
    ]);

    let drain_report = decode_loop
        .begin_end_of_stream_drain_for_test(9)
        .expect("EOF drain should send NULL packet and drain tail frames");

    assert_eq!(decode_loop.codec_api.flush_buffers_count, 1);
    assert_eq!(decode_loop.codec_api.end_of_stream_send_count, 1);
    assert_eq!(drain_report.frames.len(), 1);
    assert_eq!(
        drain_report.state,
        VideoDecoderEndOfStreamDrainState::Drained { generation: 9 }
    );
}

#[test]
fn eof_from_normal_packet_send_is_protocol_violation() {
    let mut decode_loop = fake_loop([FakeSendResult::EndOfFile], [FakeReceiveResult::Again]);
    let error = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect_err("normal packet send must not be treated as EOF drain");

    assert!(matches!(
        error,
        FfmpegDecoderThreadError::ProtocolViolation { .. }
    ));
    assert_eq!(decode_loop.completed_packet_count, 0);
}

#[test]
fn fatal_receive_error_propagates_without_packet_completion() {
    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [FakeReceiveResult::Fatal("fake receive failed")],
    );
    let error = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect_err("fatal receive should stop the decode loop");

    assert_eq!(
        error,
        FfmpegDecoderThreadError::ProtocolViolation {
            reason: "fake receive failed".to_owned()
        }
    );
    assert_eq!(decode_loop.completed_packet_count, 0);
}

#[test]
fn pts_policy_uses_best_effort_then_pts_then_interpolation() {
    let mut decode_loop = fake_loop(
        [
            FakeSendResult::Accepted,
            FakeSendResult::Accepted,
            FakeSendResult::Accepted,
        ],
        [
            FakeReceiveResult::Frame(frame_timestamps(5, 6, 2)),
            FakeReceiveResult::Again,
            FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, 8, 2)),
            FakeReceiveResult::Again,
            FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, NO_TIMESTAMP, 2)),
            FakeReceiveResult::Again,
        ],
    );

    let first = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 0, Duration::ZERO))
        .expect("best effort frame should decode");
    let second = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 1, Duration::from_millis(1)))
        .expect("pts fallback frame should decode");
    let third = decode_loop
        .send_packet_for_test(decode_packet_with_pts(1, 2, Duration::from_millis(2)))
        .expect("interpolated frame should decode");

    assert_eq!(first.frames[0].pts, Duration::from_millis(5));
    assert_eq!(first.frames[0].generation, 1);
    assert_eq!(second.frames[0].pts, Duration::from_millis(8));
    assert_eq!(third.frames[0].pts, Duration::from_millis(10));
}

#[test]
fn pts_only_packet_time_base_resolves_materialized_frame_timestamp() {
    let track_id = media_core::TrackId::new(1);
    let time_base = media_core::TimeBase::new(1, 90_000).expect("валидная MPEG time base");
    let mut decode_packet = decode_packet_with_pts(7, 18_000, Duration::from_millis(200));
    decode_packet.track_pts = Some(media_core::TrackTimestamp::new(track_id, 18_000, time_base));
    decode_packet.track_dts = None;

    let mut decode_loop = fake_loop(
        [FakeSendResult::Accepted],
        [
            FakeReceiveResult::Frame(frame_timestamps(18_000, NO_TIMESTAMP, 18_000)),
            FakeReceiveResult::Again,
        ],
    );

    let progress = decode_loop
        .send_packet_for_test(decode_packet)
        .expect("PTS-only packet должен materialize frame");

    assert_eq!(progress.frames.len(), 1);
    assert_eq!(progress.frames[0].generation, 7);
    assert_eq!(progress.frames[0].pts, Duration::from_millis(200));
}

#[cfg(feature = "ffmpeg")]
#[test]
fn avframe_resource_remains_readable_until_release() {
    let provider = FfmpegHostResourceProvider::new_for_test(4);
    let mut frame = test_yuv420_frame(2, 2, 32);
    frame
        .write_test_plane_row(0, 0, &[10, 11])
        .expect("Y row 0 should be writable");
    frame
        .write_test_plane_row(0, 1, &[20, 21])
        .expect("Y row 1 should be writable");

    let publication = provider
        .insert_frame(
            3,
            frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect("valid AVFrame should become a host resource");
    let descriptor = lookup_host_planar_descriptor(&provider, publication.handle);

    assert_eq!(
        descriptor
            .visible_plane_row_bytes(0, 1)
            .expect("Y row remains readable"),
        &[20, 21]
    );
    assert!(matches!(
        provider.resource_lookup(publication.handle),
        PresentFrameResourceProviderLookup::Ready { .. }
    ));

    provider.release_frame(publication.handle);

    assert!(matches!(
        provider.resource_lookup(publication.handle),
        PresentFrameResourceProviderLookup::Missing { .. }
    ));
}

#[cfg(feature = "ffmpeg")]
#[test]
fn release_drops_resource_entry_and_stale_release_is_noop() {
    let provider = FfmpegHostResourceProvider::new_for_test(2);
    let frame = test_yuv420_frame(2, 2, 32);
    let publication = provider
        .insert_frame(
            1,
            frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect("valid AVFrame should be inserted");

    provider.release_frame(publication.handle);
    provider.release_frame(publication.handle);

    assert!(matches!(
        provider.resource_descriptor_lookup(publication.handle),
        PresentFrameResourceDescriptorLookup::Missing { .. }
    ));
    assert_eq!(
        provider.snapshot(0).host_frames_in_flight,
        0,
        "release should remove the provider-owned entry"
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn descriptor_clone_keeps_avframe_owner_without_copying_planes() {
    let provider = FfmpegHostResourceProvider::new_for_test(2);
    let mut frame = test_yuv420_frame(2, 2, 32);
    frame
        .write_test_plane_row(0, 0, &[7, 8])
        .expect("Y row should be writable");
    let publication = provider
        .insert_frame(
            1,
            frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect("valid AVFrame should be inserted");
    let descriptor = match provider.resource_descriptor_lookup(publication.handle) {
        PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
        other => panic!("expected ready descriptor lookup, got {other:?}"),
    };
    let cloned_descriptor = descriptor
        .try_clone_for_lookup()
        .expect("host-planar descriptor clone should not duplicate plane bytes");

    provider.release_frame(publication.handle);

    let FrameResourceDescriptor::HostPlanar(cloned_descriptor) = cloned_descriptor else {
        panic!("expected host-planar cloned descriptor");
    };
    assert_eq!(
        cloned_descriptor
            .visible_plane_row_bytes(0, 0)
            .expect("cloned descriptor keeps AVFrame owner alive"),
        &[7, 8]
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn invalid_linesize_and_data_are_rejected() {
    let provider = FfmpegHostResourceProvider::new_for_test(2);
    let mut invalid_linesize_frame = test_yuv420_frame(2, 2, 32);
    invalid_linesize_frame.set_test_linesize(0, 1);

    let linesize_error = provider
        .insert_frame(
            1,
            invalid_linesize_frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect_err("visible row wider than linesize must be rejected");
    assert!(
        linesize_error
            .to_string()
            .contains("AVFrame HostPlanar descriptor validation")
    );

    let mut null_data_frame = test_yuv420_frame(2, 2, 32);
    null_data_frame.clear_test_plane_data(1);
    let data_error = provider
        .insert_frame(
            1,
            null_data_frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect_err("null AVFrame plane data must be rejected");
    assert!(
        data_error
            .to_string()
            .contains("AVFrame HostPlanar descriptor validation")
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn unsupported_avframe_format_is_rejected_with_diagnostic_context() {
    let provider = FfmpegHostResourceProvider::new_for_test(2);
    let unsupported_frame = OwnedAvFrame::new_test_unsupported_nv12_frame(2, 2, 32)
        .expect("test NV12 AVFrame allocation should succeed");

    let error = provider
        .insert_frame(
            1,
            unsupported_frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect_err("NV12 must not enter the HostPlanar software resource table");

    assert!(
        error
            .to_string()
            .contains("not a supported v1 software planar YUV format")
    );
    assert_eq!(provider.snapshot(0).upload_failures, 1);
}

#[cfg(feature = "ffmpeg")]
#[test]
fn padded_avframe_linesize_reads_visible_bytes_and_validates_descriptor() {
    let provider = FfmpegHostResourceProvider::new_for_test(2);
    let mut frame = test_yuv420_frame(3, 3, 32);
    frame
        .write_test_plane_row(0, 0, &[1, 2, 3])
        .expect("Y row 0 should be writable");
    frame
        .write_test_plane_row(0, 2, &[9, 10, 11])
        .expect("Y row 2 should be writable");
    frame
        .write_test_plane_row(1, 1, &[21, 22])
        .expect("U row should be writable");
    frame
        .write_test_plane_row(2, 1, &[31, 32])
        .expect("V row should be writable");

    let publication = provider
        .insert_frame(
            5,
            frame,
            host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
        )
        .expect("padded AVFrame should validate");
    let descriptor = lookup_host_planar_descriptor(&provider, publication.handle);

    assert!(
        descriptor.planes[0].stride > 3,
        "FFmpeg test frame should expose row padding through linesize"
    );
    assert_eq!(
        descriptor
            .visible_plane_row_bytes(0, 0)
            .expect("visible Y row should ignore right padding"),
        &[1, 2, 3]
    );
    assert_eq!(
        descriptor
            .visible_plane_row_bytes(1, 1)
            .expect("visible U row should ignore right padding"),
        &[21, 22]
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn h264_avcc_config_passes_codec_private_as_extradata() {
    let avcc = Bytes::from_static(&[0x01, 0x64, 0x00, 0x1f, 0xff, 0xe1]);
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H264,
        Some(avcc.clone()),
        Some(VideoStreamPacketization::H264(
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: codec_core::H264NalLengthSize::FOUR,
            },
        )),
    );

    let extradata = extradata_for_stream_config(&config)
        .expect("avcC config is valid")
        .expect("length-prefixed H.264 must carry extradata");

    assert_eq!(extradata.as_slice(), avcc.as_ref());
}

#[cfg(feature = "ffmpeg")]
#[test]
fn h264_avc3_config_passes_minimal_codec_private_as_extradata() {
    let avc3_configuration = Bytes::from_static(&[0x01, 0x4d, 0x40, 0x1f, 0xff, 0xe0, 0x00]);
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H264,
        Some(avc3_configuration.clone()),
        Some(VideoStreamPacketization::H264(
            H264Packetization::AvccLengthPrefixedWithInBandParameterSets {
                nal_length_size: codec_core::H264NalLengthSize::FOUR,
            },
        )),
    );

    let extradata = extradata_for_stream_config(&config)
        .expect("avc3 config должен быть валиден")
        .expect("length-prefixed avc3 должен передать decoder configuration");

    assert_eq!(extradata.as_slice(), avc3_configuration.as_ref());
}

#[cfg(feature = "ffmpeg")]
#[test]
fn h265_hvcc_config_passes_codec_private_as_extradata() {
    let hvcc = Bytes::from_static(&[0x01, 0x01, 0x60, 0x00, 0x00, 0x00]);
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H265,
        Some(hvcc.clone()),
        Some(VideoStreamPacketization::H265(
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: codec_core::H265NalLengthSize::FOUR,
            },
        )),
    );

    let extradata = extradata_for_stream_config(&config)
        .expect("hvcC config is valid")
        .expect("length-prefixed H.265 must carry extradata");

    assert_eq!(extradata.as_slice(), hvcc.as_ref());
}

#[cfg(feature = "ffmpeg")]
#[test]
fn h264_annexb_config_does_not_pass_extradata() {
    // Annex B несёт SPS/PPS in-band; передача avcC переключила бы decoder в
    // length-prefixed режим и сломала бы парсинг, поэтому extradata = None.
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H264,
        Some(Bytes::from_static(&[0x01, 0x64, 0x00, 0x1f])),
        Some(VideoStreamPacketization::H264(H264Packetization::AnnexB)),
    );

    assert_eq!(
        extradata_for_stream_config(&config).expect("annexb config is valid"),
        None
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn length_prefixed_without_codec_private_is_typed_invalid() {
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H264,
        None,
        Some(VideoStreamPacketization::H264(
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: codec_core::H264NalLengthSize::FOUR,
            },
        )),
    );

    let error = extradata_for_stream_config(&config).expect_err("missing avcC must be rejected");

    match error {
        FfmpegOpenDecoderError::Unsupported(VideoStreamConfigRejection::InvalidCodecPrivate {
            codec,
            ..
        }) => assert_eq!(codec, codec_core::VideoCodec::H264),
        other => panic!("expected typed InvalidCodecPrivate rejection, got {other:?}"),
    }
}

#[cfg(feature = "ffmpeg")]
#[test]
fn length_prefixed_with_empty_codec_private_is_typed_invalid() {
    let config = extradata_test_stream_config(
        codec_core::VideoCodec::H265,
        Some(Bytes::new()),
        Some(VideoStreamPacketization::H265(
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: codec_core::H265NalLengthSize::FOUR,
            },
        )),
    );

    let error = extradata_for_stream_config(&config).expect_err("empty hvcC must be rejected");

    assert!(matches!(
        error,
        FfmpegOpenDecoderError::Unsupported(VideoStreamConfigRejection::InvalidCodecPrivate { .. })
    ));
}
