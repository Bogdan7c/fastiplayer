//! Нейтральное владение candidate video backend-ом до atomic install commit.
//!
//! Этот модуль намеренно не знает о player session, WGPU, renderer generation или
//! concrete decoder crate-ах. Он фиксирует только ownership decoder half-а и
//! fake-able request/reply boundary, который Session 00C1 сможет связать с
//! app-owned candidate slot-ом.

use std::fmt;

use video_core::{
    DecodeThreadError, VideoDecoderControlBackpressureReason, VideoStreamConfigRejection,
    VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_frame_contract::VideoFrameContract;

use crate::StartedVideoBackend;

/// Запущенный decoder backend, который ещё не принадлежит active playback pipeline.
///
/// Отдельный тип не позволяет случайно передать candidate в существующий active
/// install boundary до успешной stream configuration.
pub struct DetachedVideoBackend {
    /// Старый startup artifact остаётся полностью owned внутри neutral typestate.
    started_backend: StartedVideoBackend,
}

impl DetachedVideoBackend {
    /// Переводит только что запущенный backend в explicit detached ownership.
    #[must_use]
    pub fn from_started(started_backend: StartedVideoBackend) -> Self {
        // Единственное действие здесь — смена ownership-типа без запуска работы.
        Self { started_backend }
    }

    /// Возвращает canonical backend ID для проверки matching resource plan-а.
    #[must_use]
    pub fn backend_id(&self) -> &str {
        // ID заимствуется без раскрытия concrete decoder handle-а.
        self.started_backend.backend_id()
    }

    /// Fallible настраивает detached decoder и возвращает installable typestate.
    ///
    /// Любой отказ потребляет и освобождает detached backend ровно один раз.
    pub fn configure_stream(
        self,
        stream_config: VideoStreamDecodeConfig,
    ) -> Result<ConfiguredDetachedVideoBackend, DetachedVideoBackendConfigurationError> {
        // Configuration выполняется на candidate decoder-е, не на active pipeline.
        let configuration_result = self
            .started_backend
            .decoder_thread
            .configure_stream(stream_config);

        // Каждая protocol-семантика сохраняется отдельным typed outcome-ом.
        match configuration_result {
            // Новый backend может сообщить Configured после принятия stream config.
            VideoStreamConfigResult::Configured
            // Эквивалентный заранее установленный config также готов к commit.
            | VideoStreamConfigResult::Unchanged => Ok(ConfiguredDetachedVideoBackend {
                // Ownership переносится без повторного startup или allocation.
                started_backend: self.started_backend,
            }),
            // Detached wrapper всегда содержит decoder; отсутствие — protocol failure.
            VideoStreamConfigResult::AbsentDecoder => {
                Err(DetachedVideoBackendConfigurationError::AbsentDecoder)
            }
            // Configure не должен очищать stream и не маскирует этот outcome успехом.
            VideoStreamConfigResult::Cleared => {
                Err(DetachedVideoBackendConfigurationError::UnexpectedClear)
            }
            // Unsupported сохраняет точную neutral rejection-причину.
            VideoStreamConfigResult::Unsupported(rejection) => {
                Err(DetachedVideoBackendConfigurationError::Unsupported(rejection))
            }
            // Backpressure остаётся retry-policy-neutral typed failure текущего request-а.
            VideoStreamConfigResult::Backpressure(reason) => {
                Err(DetachedVideoBackendConfigurationError::Backpressure(reason))
            }
            // Fatal decoder error никогда не сворачивается в unavailable/bool.
            VideoStreamConfigResult::Fatal(error) => {
                Err(DetachedVideoBackendConfigurationError::Fatal(error))
            }
        }
    }
}

/// Detached backend с уже успешно подготовленным stream state.
pub struct ConfiguredDetachedVideoBackend {
    /// Backend остаётся detached до explicit будущего player commit-а.
    started_backend: StartedVideoBackend,
}

impl ConfiguredDetachedVideoBackend {
    /// Возвращает canonical backend ID без раскрытия concrete implementation.
    #[must_use]
    pub fn backend_id(&self) -> &str {
        // Диагностика использует тот же ID, что capability selection.
        self.started_backend.backend_id()
    }

    /// Передаёт заранее настроенный backend будущей infallible install boundary.
    #[must_use]
    pub fn into_started_backend(self) -> StartedVideoBackend {
        // Здесь нет startup/configuration: перемещается только готовый pointer owner.
        self.started_backend
    }
}

/// Typed отказ fallible stream configuration detached decoder-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedVideoBackendConfigurationError {
    /// Decoder неожиданно отсутствовал внутри уже созданного detached wrapper-а.
    AbsentDecoder,

    /// Backend неожиданно очистил stream вместо его configuration.
    UnexpectedClear,

    /// Backend жив, но не поддерживает точный stream contract.
    Unsupported(VideoStreamConfigRejection),

    /// Bounded decoder control queue не приняла configuration command.
    Backpressure(VideoDecoderControlBackpressureReason),

    /// Decoder/backend завершил configuration fatal ошибкой.
    Fatal(DecodeThreadError),
}

impl fmt::Display for DetachedVideoBackendConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Текст сохраняет variant и техническую причину для diagnostics.
        match self {
            Self::AbsentDecoder => formatter.write_str("detached decoder unexpectedly absent"),
            Self::UnexpectedClear => {
                formatter.write_str("detached decoder cleared stream during configuration")
            }
            Self::Unsupported(rejection) => {
                write!(formatter, "detached decoder rejected stream: {rejection:?}")
            }
            Self::Backpressure(reason) => {
                write!(
                    formatter,
                    "detached decoder configuration backpressure: {reason:?}"
                )
            }
            Self::Fatal(error) => {
                write!(formatter, "detached decoder configuration failed: {error}")
            }
        }
    }
}

impl std::error::Error for DetachedVideoBackendConfigurationError {}

/// Точный renderer-neutral выбор video output-а, сделанный player preflight-ом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedVideoBackendSelection {
    /// Canonical backend ID выбранного playable output-а.
    expected_backend_id: String,

    /// Exact decoder-to-renderer contract выбранного stream plan-а.
    frame_contract: VideoFrameContract,
}

impl DetachedVideoBackendSelection {
    /// Создаёт exact selection из capability-intersected player plan-а.
    #[must_use]
    pub fn selected(
        expected_backend_id: impl Into<String>,
        frame_contract: VideoFrameContract,
    ) -> Self {
        Self {
            expected_backend_id: expected_backend_id.into(),
            frame_contract,
        }
    }

    /// Возвращает canonical backend ID, уже выбранный player capability layer-ом.
    #[must_use]
    pub fn expected_backend_id(&self) -> &str {
        &self.expected_backend_id
    }

    /// Возвращает exact frame contract выбранного stream plan-а.
    #[must_use]
    pub const fn frame_contract(&self) -> VideoFrameContract {
        self.frame_contract
    }
}

/// Запрос player-side candidate-а на detached decoder half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedVideoBackendRequest<RequestId> {
    /// Correlation ID принадлежит caller-у и остаётся generic для neutral crate-а.
    request_id: RequestId,

    /// Player-selected output/stream intent запрещает app делать независимый выбор.
    selection: DetachedVideoBackendSelection,
}

impl<RequestId> DetachedVideoBackendRequest<RequestId> {
    /// Создаёт request без playlist/app-specific payload-а.
    #[must_use]
    pub const fn new(request_id: RequestId, selection: DetachedVideoBackendSelection) -> Self {
        Self {
            request_id,
            selection,
        }
    }

    /// Возвращает исходную correlation identity.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        // Ссылка не требует от generic ID свойства Copy.
        &self.request_id
    }

    /// Возвращает exact player-selected output/stream intent.
    #[must_use]
    pub const fn selection(&self) -> &DetachedVideoBackendSelection {
        &self.selection
    }

    /// Разделяет correlation ID и selection без повторного планирования.
    #[must_use]
    pub fn into_parts(self) -> (RequestId, DetachedVideoBackendSelection) {
        (self.request_id, self.selection)
    }
}

/// Ответ app resource owner-а на exact detached backend request.
pub struct DetachedVideoBackendReply<RequestId> {
    /// Exact ID позволяет caller-у отвергнуть stale/mismatched reply.
    request_id: RequestId,

    /// Success передаёт единственный detached owner; failure не создаёт fake handle.
    result: Result<DetachedVideoBackend, DetachedVideoBackendResourceError>,
}

impl<RequestId> DetachedVideoBackendReply<RequestId> {
    /// Создаёт successful reply с единственным detached backend owner-ом.
    #[must_use]
    pub fn available(request_id: RequestId, backend: DetachedVideoBackend) -> Self {
        // Backend перемещается в reply и не клонируется.
        Self {
            request_id,
            result: Ok(backend),
        }
    }

    /// Создаёт typed failed reply без destructive fallback.
    #[must_use]
    pub fn unavailable(request_id: RequestId, error: DetachedVideoBackendResourceError) -> Self {
        // Ошибка остаётся request-correlated и lossless.
        Self {
            request_id,
            result: Err(error),
        }
    }

    /// Возвращает correlation identity ответа.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        // Заимствование не потребляет единственный backend owner.
        &self.request_id
    }

    /// Разделяет correlation identity и owned typed result.
    #[must_use = "reply identity and detached backend result must both be handled"]
    pub fn into_parts(
        self,
    ) -> (
        RequestId,
        Result<DetachedVideoBackend, DetachedVideoBackendResourceError>,
    ) {
        // Оба поля перемещаются без startup/configuration side effects.
        (self.request_id, self.result)
    }
}

/// Typed причина, почему app owner не выдал candidate decoder half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedVideoBackendResourceError {
    /// Единственный bounded candidate slot занят другим request-ом/outcome-ом.
    AdmissionBackpressure {
        /// Диагностика объясняет occupied owner state без hidden retry policy.
        reason: String,
    },

    /// Выбранный backend/runtime недоступен для этого candidate request-а.
    Unavailable {
        /// Диагностическая причина не используется как control-flow identity.
        reason: String,
    },

    /// Runtime/driver не разрешил временный второй decoder resource set.
    ResourceExhausted {
        /// Причина подтверждает bounded resource отказ без fallback-а.
        reason: String,
    },

    /// Concrete factory не смогла запустить candidate backend.
    StartupFailed {
        /// Canonical backend ID связывает failure с выбранным resource plan-ом.
        backend_id: String,

        /// Concrete error остаётся диагностикой за neutral boundary.
        message: String,
    },
}

impl fmt::Display for DetachedVideoBackendResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Все resource failures остаются различимыми в логах и tests.
        match self {
            Self::AdmissionBackpressure { reason } => {
                write!(
                    formatter,
                    "candidate backend admission backpressure: {reason}"
                )
            }
            Self::Unavailable { reason } => {
                write!(formatter, "candidate backend unavailable: {reason}")
            }
            Self::ResourceExhausted { reason } => {
                write!(formatter, "candidate backend resources exhausted: {reason}")
            }
            Self::StartupFailed {
                backend_id,
                message,
            } => write!(
                formatter,
                "candidate backend {backend_id} startup failed: {message}"
            ),
        }
    }
}

impl std::error::Error for DetachedVideoBackendResourceError {}

/// Terminal причина отмены одной detached candidate transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachedVideoBackendCandidateCancellationCause {
    /// Более новый request вытеснил текущий candidate.
    Superseded,

    /// Candidate принадлежит уже неактуальному renderer generation.
    StaleRendererGeneration,

    /// Renderer lifecycle перешёл в suspended до commit barrier.
    RendererSuspended,

    /// Resource port/worker disconnect разорвал handoff.
    Disconnected,

    /// Явная отмена владельцем request-а до commit barrier.
    Requested,
}

/// Player-side status после получения detached backend half-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedVideoBackendCandidateStatus<RequestId> {
    /// Stream configuration завершилась успешно; backend остаётся у player candidate-а.
    StreamConfigured {
        /// Exact correlation identity исходного request-а.
        request_id: RequestId,

        /// Canonical backend ID защищает matching pair от ошибочного смешивания.
        backend_id: String,
    },

    /// Fallible stream configuration завершилась typed failure до commit barrier.
    ConfigurationFailed {
        /// Exact correlation identity исходного request-а.
        request_id: RequestId,

        /// Точная neutral configuration failure.
        error: DetachedVideoBackendConfigurationError,
    },

    /// Player half освобождён из-за typed cancellation.
    Cancelled {
        /// Exact correlation identity исходного request-а.
        request_id: RequestId,

        /// Terminal cancellation cause без общего ambiguous bool.
        cause: DetachedVideoBackendCandidateCancellationCause,
    },
}

impl<RequestId> DetachedVideoBackendCandidateStatus<RequestId> {
    /// Возвращает correlation identity любого status variant-а.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        // Pattern сохраняет единый accessor без копирования ID.
        match self {
            Self::StreamConfigured { request_id, .. }
            | Self::ConfigurationFailed { request_id, .. }
            | Self::Cancelled { request_id, .. } => request_id,
        }
    }
}

/// Typed disconnect fake-able request/reply port-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachedVideoBackendPortError;

impl fmt::Display for DetachedVideoBackendPortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Port error не маскируется под backend/configuration failure.
        formatter.write_str("detached video backend resource port disconnected")
    }
}

impl std::error::Error for DetachedVideoBackendPortError {}

/// Fake-able request/reply boundary между player candidate и app resource owner-ом.
///
/// Trait не задаёт transport, thread или queue implementation и поэтому подходит
/// как deterministic fake, так и будущему bounded channel adapter-у Session 00C1.
pub trait DetachedVideoBackendResourcePort {
    /// Neutral crate не навязывает конкретный install/request ID owner-у.
    type RequestId: Clone + Eq;

    /// Запрашивает единственный detached backend half по exact correlation ID.
    fn request_detached_backend(
        &mut self,
        request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError>;

    /// Публикует app owner-у matching configured/failure/cancel status.
    fn publish_candidate_status(
        &mut self,
        status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError>;

    /// Просит player owner terminal-cancel-ить и освободить exact candidate half.
    fn cancel_candidate(
        &mut self,
        request_id: Self::RequestId,
        cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError>;
}

#[cfg(test)]
mod tests {
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
                VideoStreamConfigResult::Unsupported(
                    VideoStreamConfigRejection::BackendUnsupported {
                        reason: "unsupported fake stream".to_owned(),
                    },
                ),
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
        ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError>
        {
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
}
