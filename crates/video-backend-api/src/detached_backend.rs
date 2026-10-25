//! Нейтральное владение candidate video backend-ом до atomic install commit.
//!
//! Этот модуль намеренно не знает о player session, WGPU, renderer generation или
//! concrete decoder crate-ах. Он фиксирует только ownership decoder half-а и
//! fake-able request/reply boundary, который Session 00C1 сможет связать с
//! app-owned candidate slot-ом.

use std::fmt;

use codec_core::VideoDecodeRequirement;
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
///
/// `PartialEq` без `Eq`: требование потока содержит `f64` (fps).
#[derive(Debug, Clone, PartialEq)]
pub struct DetachedVideoBackendSelection {
    /// Canonical backend ID выбранного playable output-а.
    expected_backend_id: String,

    /// Exact decoder-to-renderer contract выбранного stream plan-а.
    frame_contract: VideoFrameContract,

    /// Требование видеопотока, под которое player выбрал этот backend.
    ///
    /// `None` — отправитель его не указал (приложение тогда прежнее требование не меняет).
    stream_requirement: Option<VideoDecodeRequirement>,
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
            stream_requirement: None,
        }
    }

    /// Прикладывает требование видеопотока, под которое сделан выбор backend-а.
    #[must_use]
    pub fn with_stream_requirement(mut self, requirement: VideoDecodeRequirement) -> Self {
        self.stream_requirement = Some(requirement);
        self
    }

    /// Требование видеопотока, если отправитель его приложил.
    #[must_use]
    pub const fn stream_requirement(&self) -> Option<&VideoDecodeRequirement> {
        self.stream_requirement.as_ref()
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
#[derive(Debug, Clone, PartialEq)]
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
mod tests;
