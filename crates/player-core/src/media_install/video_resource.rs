//! Request-scoped video backend policy и app resource transport strong media install-а.

use codec_core::DecodeBackendId;

use super::MediaInstallRequestId;

/// Type-erased transport half; policy и lifecycle остаются у enclosing wrapper-а.
type DetachedMediaInstallVideoResourcePort =
    dyn video_backend_api::DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>
        + Send;

/// Ограничение app-owned backend policy для одного media install request-а.
///
/// App задаёт только допустимое множество backend-ов. Exact backend и frame contract
/// по-прежнему выбирает player из capability-intersected output-ов candidate-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaInstallVideoBackendConstraint {
    /// Разрешает первый playable output в нейтральном capability snapshot-е.
    AnyPlayable,

    /// Запрещает fallback на любой backend, кроме явно разрешённого app policy.
    RequireBackend(DecodeBackendId),
}

impl MediaInstallVideoBackendConstraint {
    /// Проверяет exact backend ID без раскрытия способа хранения constraint-а caller-у.
    #[must_use]
    pub fn allows_backend_id(&self, backend_id: &str) -> bool {
        match self {
            Self::AnyPlayable => true,
            Self::RequireBackend(required_backend_id) => required_backend_id.as_str() == backend_id,
        }
    }
}

/// Request-scoped player boundary к заранее staged app half video candidate-а.
///
/// `Send` требуется только потому, что owner перемещается в player worker thread;
/// concrete renderer/materializer pointers через этот boundary в player не проходят.
pub struct MediaInstallVideoResourcePort {
    /// Immutable policy snapshot exact media install request-а.
    backend_constraint: MediaInstallVideoBackendConstraint,

    /// Fake-able request/reply/status/cancel transport к app resource owner-у.
    resource_port: Box<DetachedMediaInstallVideoResourcePort>,
}

impl MediaInstallVideoResourcePort {
    /// Связывает app-owned constraint и concrete transport в один linear request boundary.
    #[must_use]
    pub fn new<ResourcePort>(
        backend_constraint: MediaInstallVideoBackendConstraint,
        resource_port: ResourcePort,
    ) -> Self
    where
        ResourcePort: video_backend_api::DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>
            + Send
            + 'static,
    {
        Self::from_boxed(backend_constraint, Box::new(resource_port))
    }

    /// Сохраняет старый type-erased adapter path без повторного concrete boxing-а.
    #[must_use]
    pub fn from_boxed(
        backend_constraint: MediaInstallVideoBackendConstraint,
        resource_port: Box<
            dyn video_backend_api::DetachedVideoBackendResourcePort<
                    RequestId = MediaInstallRequestId,
                > + Send,
        >,
    ) -> Self {
        Self {
            backend_constraint,
            resource_port,
        }
    }

    /// Compatibility helper для callers без app-owned backend preference.
    #[must_use]
    pub fn any_playable<ResourcePort>(resource_port: ResourcePort) -> Self
    where
        ResourcePort: video_backend_api::DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>
            + Send
            + 'static,
    {
        Self::new(
            MediaInstallVideoBackendConstraint::AnyPlayable,
            resource_port,
        )
    }

    /// Возвращает immutable constraint, который player обязан применить до resource request-а.
    #[must_use]
    pub const fn backend_constraint(&self) -> &MediaInstallVideoBackendConstraint {
        &self.backend_constraint
    }

    /// Даёт owner-коду intent-revealing mutable доступ только к transport half-у.
    fn resource_port_mut(&mut self) -> &mut DetachedMediaInstallVideoResourcePort {
        self.resource_port.as_mut()
    }
}

impl video_backend_api::DetachedVideoBackendResourcePort for MediaInstallVideoResourcePort {
    type RequestId = MediaInstallRequestId;

    /// Защищает app resource owner от backend request-а вне request-scoped policy.
    fn request_detached_backend(
        &mut self,
        request: video_backend_api::DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<
        video_backend_api::DetachedVideoBackendReply<Self::RequestId>,
        video_backend_api::DetachedVideoBackendPortError,
    > {
        if !self
            .backend_constraint
            .allows_backend_id(request.selection().expected_backend_id())
        {
            return Ok(video_backend_api::DetachedVideoBackendReply::unavailable(
                *request.request_id(),
                video_backend_api::DetachedVideoBackendResourceError::Unavailable {
                    reason: format!(
                        "Backend `{}` запрещён request-scoped media install policy",
                        request.selection().expected_backend_id()
                    ),
                },
            ));
        }
        self.resource_port_mut().request_detached_backend(request)
    }

    /// Делегирует matching configured/failure/cancel status exact app owner-у.
    fn publish_candidate_status(
        &mut self,
        status: video_backend_api::DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), video_backend_api::DetachedVideoBackendPortError> {
        self.resource_port_mut().publish_candidate_status(status)
    }

    /// Делегирует terminal cancellation exact app owner-у.
    fn cancel_candidate(
        &mut self,
        request_id: Self::RequestId,
        cause: video_backend_api::DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), video_backend_api::DetachedVideoBackendPortError> {
        self.resource_port_mut().cancel_candidate(request_id, cause)
    }
}
