use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use codec_core::{VideoCodec, VideoDisplayOrientation};
use media_core::TrackId;
use video_frame_contract::VideoFrameContract;

use super::*;
use crate::PresentFrameResourceProviderHandle;

/// Decoder fake хранит exact protocol outcome и drop counter ownership-проверки.
struct ConfigurableFakeDecoder {
    /// Каждый вызов configuration возвращает заранее выбранный neutral outcome.
    configuration_result: VideoStreamConfigResult,

    /// Counter позволяет доказать exactly-once release decoder half-а.
    drop_count: Arc<AtomicUsize>,
}

impl Drop for ConfigurableFakeDecoder {
    fn drop(&mut self) {
        // Единственный decoder owner увеличивает counter при фактическом release.
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

impl video_core::VideoDecoderThreadHandle for ConfigurableFakeDecoder {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        // Stable имя используется только diagnostics contract-ом fake-а.
        "detached configurable fake"
    }

    fn send_packet(
        &self,
        _packet: video_core::DecodePacket,
    ) -> Result<(), video_core::DecodeSendError> {
        // Session 00C не отправляет packets до install commit.
        Err(video_core::DecodeSendError::Fatal(DecodeThreadError::new(
            "detached fake does not accept packets",
        )))
    }

    fn configure_stream(&self, _config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        // Clone сохраняет exact typed variant для deterministic test-а.
        self.configuration_result.clone()
    }

    fn release_frame(&self, _handle: video_core::FrameResourceHandle) {
        // Fake не создаёт decoded resources до candidate commit.
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        // Candidate configuration не публикует frames.
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        // В этих focused tests decoder diagnostics отсутствуют.
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        // Configuration outcome передаёт ошибку синхронно.
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        // Fake flush не меняет ownership или configuration result.
        Ok(())
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        // Renderer provider не должен запрашиваться neutral configuration boundary.
        panic!("detached configuration test must not request renderer provider")
    }

    fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        // Fake не резервирует decoder surfaces.
        None
    }

    fn packet_queue_depth(&self) -> usize {
        // Packet queue не используется до install commit.
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        // Packet completion отсутствует без packet submission.
        0
    }
}

/// Строит минимальный валидный host-upload stream contract для configuration tests.
fn sample_stream_config() -> VideoStreamDecodeConfig {
    // Поля отражают уже выбранный renderer-intersected software output.
    VideoStreamDecodeConfig {
        track_id: TrackId::new(7),
        codec: VideoCodec::Vp9,
        profile: None,
        bit_depth: None,
        chroma: None,
        coded_width: Some(1920),
        coded_height: Some(1080),
        display_orientation: VideoDisplayOrientation::Identity,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        codec_private: None,
        packetization: None,
    }
}

/// Создаёт detached backend с exact configuration outcome и shared drop counter.
fn detached_backend(
    configuration_result: VideoStreamConfigResult,
    drop_count: Arc<AtomicUsize>,
) -> DetachedVideoBackend {
    // Fake проходит тот же StartedVideoBackend wrapper, что concrete factories.
    DetachedVideoBackend::from_started(StartedVideoBackend::from_decoder_thread(
        "fake-detached",
        ConfigurableFakeDecoder {
            configuration_result,
            drop_count,
        },
    ))
}

#[test]
fn configured_detached_backend_requires_explicit_installable_typestate_conversion() {
    // Counter начинается с нуля до создания/destroy decoder owner-а.
    let drop_count = Arc::new(AtomicUsize::new(0));
    // Candidate configuration использует успешный protocol outcome.
    let detached = detached_backend(VideoStreamConfigResult::Configured, drop_count.clone());

    // Успех возвращает отдельный configured typestate.
    let configured = detached
        .configure_stream(sample_stream_config())
        .unwrap_or_else(|error| panic!("configuration must succeed: {error}"));
    // Canonical backend ID сохраняется через typestate transition.
    assert_eq!(configured.backend_id(), "fake-detached");
    // До drop configured owner decoder остаётся жив.
    assert_eq!(drop_count.load(Ordering::SeqCst), 0);

    // Explicit conversion выполняет только ownership move в startup artifact.
    let installable = configured.into_started_backend();
    // Drop installable artifact освобождает decoder ровно один раз.
    drop(installable);
    // Никакого второго скрытого owner-а после conversion не остаётся.
    assert_eq!(drop_count.load(Ordering::SeqCst), 1);
}

#[test]
fn every_configuration_failure_preserves_typed_outcome_and_releases_backend_once() {
    // Проверяем все protocol branches, которые нельзя схлопнуть в bool.
    let cases = [
        (
            VideoStreamConfigResult::AbsentDecoder,
            DetachedVideoBackendConfigurationError::AbsentDecoder,
        ),
        (
            VideoStreamConfigResult::Cleared,
            DetachedVideoBackendConfigurationError::UnexpectedClear,
        ),
        (
            VideoStreamConfigResult::Unsupported(VideoStreamConfigRejection::BackendUnsupported {
                reason: "unsupported fake stream".to_owned(),
            }),
            DetachedVideoBackendConfigurationError::Unsupported(
                VideoStreamConfigRejection::BackendUnsupported {
                    reason: "unsupported fake stream".to_owned(),
                },
            ),
        ),
        (
            VideoStreamConfigResult::Backpressure(
                VideoDecoderControlBackpressureReason::ControlChannelFull {
                    queued_messages: 4,
                    capacity: 4,
                },
            ),
            DetachedVideoBackendConfigurationError::Backpressure(
                VideoDecoderControlBackpressureReason::ControlChannelFull {
                    queued_messages: 4,
                    capacity: 4,
                },
            ),
        ),
        (
            VideoStreamConfigResult::Fatal(DecodeThreadError::new("fatal fake config")),
            DetachedVideoBackendConfigurationError::Fatal(DecodeThreadError::new(
                "fatal fake config",
            )),
        ),
    ];

    // Каждый case получает независимый decoder owner и counter.
    for (configuration_result, expected_error) in cases {
        // Новый counter исключает смешивание release accounting между cases.
        let drop_count = Arc::new(AtomicUsize::new(0));
        // Fallible call потребляет detached owner.
        let result = detached_backend(configuration_result, drop_count.clone())
            .configure_stream(sample_stream_config());
        // Failed branch не должен вернуть installable backend.
        let actual_error = match result {
            Ok(_) => panic!("failed configuration must not return configured backend"),
            Err(error) => error,
        };

        // Точная typed причина пересекает boundary без потери семантики.
        assert_eq!(actual_error, expected_error);
        // Consumed failed backend освобождается ровно один раз.
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }
}

/// Fake port фиксирует exact request/status/cancel interactions без threads/channels.
struct RecordingResourcePort {
    /// Единственный prebuilt reply моделирует bounded app resource slot.
    reply: Option<DetachedVideoBackendReply<u64>>,

    /// Published status хранится losslessly для exact-correlation assertion.
    published_status: Option<DetachedVideoBackendCandidateStatus<u64>>,

    /// Cancel request хранит typed cause отдельно от player status.
    cancellation: Option<(u64, DetachedVideoBackendCandidateCancellationCause)>,
}

impl DetachedVideoBackendResourcePort for RecordingResourcePort {
    type RequestId = u64;

    fn request_detached_backend(
        &mut self,
        _request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        // Bounded fake выдаёт prebuilt reply не более одного раза.
        self.reply.take().ok_or(DetachedVideoBackendPortError)
    }

    fn publish_candidate_status(
        &mut self,
        status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        // Второй terminal/status publish считается disconnect-like protocol failure.
        if self.published_status.is_some() {
            return Err(DetachedVideoBackendPortError);
        }
        // Первый status сохраняется без coalescing.
        self.published_status = Some(status);
        Ok(())
    }

    fn cancel_candidate(
        &mut self,
        request_id: Self::RequestId,
        cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        // Exact cancellation также не может быть молча перезаписана.
        if self.cancellation.is_some() {
            return Err(DetachedVideoBackendPortError);
        }
        // Fake сохраняет request и cause как lossless pair.
        self.cancellation = Some((request_id, cause));
        Ok(())
    }
}

#[test]
fn fakeable_port_keeps_request_reply_status_and_cancel_correlated() {
    // Reply моделирует driver, который отказал только candidate resource set-у.
    let reply = DetachedVideoBackendReply::unavailable(
        41,
        DetachedVideoBackendResourceError::ResourceExhausted {
            reason: "fake driver permits only the active decoder".to_owned(),
        },
    );
    // Port содержит ровно один reply/status/cancel slot.
    let mut port = RecordingResourcePort {
        reply: Some(reply),
        published_status: None,
        cancellation: None,
    };

    // Player передаёт exact renderer-neutral selection вместе с correlation ID.
    let reply = port
        .request_detached_backend(DetachedVideoBackendRequest::new(
            41,
            DetachedVideoBackendSelection::selected(
                "ffmpeg-sw",
                VideoFrameContract::host_yuv420_planar8(),
            ),
        ))
        .expect("prebuilt reply must be available");
    // Exact ID ответа совпадает с request ID.
    assert_eq!(*reply.request_id(), 41);
    // Resource exhaustion остаётся typed и не создаёт fallback backend.
    assert!(matches!(
        reply.into_parts().1,
        Err(DetachedVideoBackendResourceError::ResourceExhausted { .. })
    ));

    // Player status публикуется с тем же request ID.
    port.publish_candidate_status(DetachedVideoBackendCandidateStatus::Cancelled {
        request_id: 41,
        cause: DetachedVideoBackendCandidateCancellationCause::Requested,
    })
    .expect("first status publish must fit bounded slot");
    // App cancellation direction сохраняет отдельную lifecycle cause.
    port.cancel_candidate(
        41,
        DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
    )
    .expect("first cancellation must fit bounded slot");

    // Ни один direction не потерял exact correlation/cause.
    assert_eq!(
        port.cancellation,
        Some((
            41,
            DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
        ))
    );
    // Terminal status также доступен ровно в одном lossless slot-е.
    assert!(matches!(
        port.published_status,
        Some(DetachedVideoBackendCandidateStatus::Cancelled {
            request_id: 41,
            cause: DetachedVideoBackendCandidateCancellationCause::Requested,
        })
    ));
}
