//! Обёртки провайдера и декодера: всё форвардится, кроме освобождения провайдера (оно идёт
//! через привязку) и `resource_provider` декодера (отдаёт обёрнутый провайдер).

use std::time::Duration;

use bytes::Bytes;
use media_core::TrackId;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodePacket, DecodeSendError, FrameResourceHandle, HostUploadResourceSnapshotStatus,
    VideoDecoderActivityWaitOutcome, VideoDecoderDiagnosticEvent,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoPrerollOutputFloor,
    VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult, VideoStreamConfigRejection,
    VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use super::fakes::{
    EVENT_TIMEOUT, FakeFence, Harness, QUIET_WINDOW, fake_control_pressure,
    fake_host_upload_snapshot, fake_resource_snapshot, lock_wait,
};
use super::trait_coverage::assert_forwarding_covers_trait;

/// Методы, которые обёртка провайдера НЕ форвардит в inner: освобождение идёт через привязку.
const PROVIDER_METHODS_NOT_FORWARDED: &[&str] = &["release_frame"];
/// Методы, которые обёртка декодера НЕ форвардит в inner: плеер получает провайдер с fence.
const DECODER_METHODS_NOT_FORWARDED: &[&str] = &["resource_provider"];

#[test]
fn provider_forwards_every_lookup_and_does_not_fall_back_to_trait_defaults() {
    let harness = Harness::new();
    let handle = FrameResourceHandle(3);
    let provider = &harness.renderer_provider;

    assert_eq!(
        provider.resource_lookup(handle),
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: lock_wait(1)
        }
    );
    assert_eq!(
        provider.try_resource_lookup(handle),
        PresentFrameResourceProviderLookup::Busy {
            resource_pool_lock_wait: lock_wait(2)
        }
    );
    assert!(matches!(
        provider.resource_descriptor_lookup(handle),
        PresentFrameResourceDescriptorLookup::Missing { resource_pool_lock_wait }
            if resource_pool_lock_wait == lock_wait(3)
    ));
    assert!(matches!(
        provider.try_resource_descriptor_lookup(handle),
        PresentFrameResourceDescriptorLookup::Fatal { resource_pool_lock_wait }
            if resource_pool_lock_wait == lock_wait(4)
    ));
    assert_eq!(
        provider.try_resource_identity_lookup(handle),
        PresentFrameResourceIdentityLookup::Missing {
            resource_pool_lock_wait: lock_wait(5)
        },
        "identity lookup не должен превращаться в дефолтный Unsupported"
    );

    assert_forwarding_covers_trait(
        "pub trait PresentFrameResourceProvider",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../video-backend-api/src/lib.rs"
        )),
        &harness.inner_log.called_provider_methods(),
        PROVIDER_METHODS_NOT_FORWARDED,
    );
}

#[test]
fn provider_release_goes_through_binding_and_waits_for_fence() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);

    harness.request_release(31);

    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), Vec::<u64>::new());
    fence.signal();
    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![31]);
}

fn sample_packet() -> DecodePacket {
    DecodePacket {
        track_id: TrackId::new(1),
        pts: Duration::ZERO,
        dts: None,
        track_pts: None,
        track_dts: None,
        generation: 0,
        encoded_bytes: Bytes::from_static(b"packet"),
        keyframe: true,
        resolved_color: None,
    }
}

fn sample_stream_config() -> VideoStreamDecodeConfig {
    VideoStreamDecodeConfig {
        track_id: TrackId::new(1),
        codec: codec_core::VideoCodec::H264,
        profile: None,
        bit_depth: None,
        chroma: None,
        coded_width: Some(2),
        coded_height: Some(2),
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        codec_private: None,
        packetization: None,
    }
}

/// Все методы декодера, кроме `resource_provider`, отдают ответы внутреннего декодера, а не
/// дефолты трейта.
#[test]
fn decoder_wrapper_forwards_every_trait_method_with_distinct_answers() {
    let mut harness = Harness::new();
    let decoder = harness
        .backend
        .take()
        .expect("backend")
        .into_decoder_thread();

    assert_eq!(decoder.backend_name(), "fake-gles2-decoder");
    assert!(matches!(
        decoder.send_packet(sample_packet()),
        Err(DecodeSendError::Fatal(error)) if error.to_string().contains("fake send_packet")
    ));
    assert!(matches!(
        decoder.configure_stream(sample_stream_config()),
        VideoStreamConfigResult::Unsupported(VideoStreamConfigRejection::BackendUnsupported { reason })
            if reason == "fake configure_stream"
    ));
    assert_eq!(decoder.clear_stream(), VideoStreamConfigResult::Cleared);
    assert_eq!(
        decoder.set_preroll_output_floor(VideoPrerollOutputFloor {
            generation: 1,
            floor_pts: Duration::ZERO,
            retain_latest_before_floor: false,
        }),
        VideoPrerollOutputFloorResult::Applied
    );
    assert_eq!(
        decoder.clear_preroll_output_floor(VideoPrerollOutputFloorClear::MatchingGeneration(1)),
        VideoPrerollOutputFloorResult::Cleared
    );
    assert_eq!(
        decoder.begin_end_of_stream_drain(41),
        VideoDecoderEndOfStreamDrainResult::Unchanged(
            VideoDecoderEndOfStreamDrainState::Draining { generation: 41 }
        )
    );
    assert_eq!(
        decoder.end_of_stream_drain_state(),
        VideoDecoderEndOfStreamDrainState::Draining { generation: 77 }
    );
    let frame = decoder.try_recv_frame().expect("кадр fake-декодера");
    assert_eq!(frame.pts, Duration::from_millis(123));
    assert!(matches!(
        decoder.try_recv_diagnostic_event(),
        Some(VideoDecoderDiagnosticEvent::FrameDropped { pts, .. }) if pts == Duration::from_millis(5)
    ));
    assert_eq!(
        decoder.try_recv_error().map(|error| error.to_string()),
        Some("fake try_recv_error".to_owned())
    );
    assert_eq!(
        decoder.flush().expect_err("fake flush").to_string(),
        "fake flush"
    );
    assert_eq!(
        decoder.decoder_resource_snapshot(),
        Some(fake_resource_snapshot())
    );
    assert_eq!(
        decoder.host_upload_resource_snapshot(),
        HostUploadResourceSnapshotStatus::Available(fake_host_upload_snapshot())
    );
    assert_eq!(
        decoder.decoder_control_channel_pressure(),
        Some(fake_control_pressure())
    );
    let activity = decoder.decoder_activity_snapshot();
    let observed_epoch = activity
        .captured_epoch()
        .expect("уведомитель активности декодера должен дойти до плеера");
    let _ = harness.activity_notifier.notify_activity();
    assert!(matches!(
        activity.activity_since(observed_epoch),
        VideoDecoderActivityWaitOutcome::ActivityReceived { .. }
    ));
    assert_eq!(decoder.packet_queue_depth(), 17);
    assert_eq!(decoder.drain_completed_packet_count(), 23);
    decoder.release_frame(FrameResourceHandle(2));
    assert_eq!(harness.inner_log.released_handles(), vec![2]);

    assert_forwarding_covers_trait(
        "pub trait VideoDecoderThreadHandle",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../video-core/src/decoder_thread.rs"
        )),
        &harness.inner_log.called_decoder_methods(),
        DECODER_METHODS_NOT_FORWARDED,
    );
}

/// `resource_provider()` отдаёт обёрнутый провайдер (освобождение через него ждёт fence), а
/// `release_frame` самого декодера идёт напрямую: кадр, не отданный рендеру, GPU не трогал.
#[test]
fn decoder_wrapper_gives_fenced_provider_but_releases_undelivered_frame_directly() {
    let mut harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    let decoder = harness
        .backend
        .take()
        .expect("backend")
        .into_decoder_thread();

    decoder
        .resource_provider()
        .release_frame(FrameResourceHandle(50));
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    assert!(
        !harness.inner_log.wait_for_release_count(1, QUIET_WINDOW),
        "освобождение через провайдер обязано ждать fence"
    );

    decoder.release_frame(FrameResourceHandle(51));
    assert_eq!(
        harness.inner_log.released_handles(),
        vec![51],
        "освобождение самим декодером идёт напрямую, не дожидаясь fence"
    );

    fence.signal();
    assert!(harness.inner_log.wait_for_release_count(2, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![51, 50]);
}

/// Декодер живёт, пока ожидающий кадр не возвращён, и уничтожается сразу после.
#[test]
fn pending_frame_keeps_decoder_alive_until_it_is_released() {
    let mut harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(60);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));

    drop(harness.backend.take());
    drop(std::mem::replace(
        &mut harness.renderer_provider,
        video_backend_api::PresentFrameResourceProviderHandle::new(NoopProvider),
    ));
    assert!(harness.decoder_dropped.try_recv().is_err());

    fence.signal();
    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert!(harness.wait_decoder_dropped());
}

/// Заглушка, чтобы выбросить настоящий провайдер из стенда.
struct NoopProvider;

impl video_backend_api::PresentFrameResourceProvider for NoopProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Missing {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}
