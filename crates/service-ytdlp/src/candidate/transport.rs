//! Provider-neutral mapping S19 request material-а в S21T open requests.

use source_core::{CancellationToken, HttpPathScope, HttpRequestTarget};
use thiserror::Error;
use web_media_core::{ContainerFamily, StreamLayout};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentIdentityError, MediaComponentRole, MediaPresentation,
    RedirectHopLimit, RedirectHopLimitError, RedirectPolicy, SecretRequestContext,
    SecretRequestScope, SourceGeneration, TransportOpenRequest, TransportOpenRequestError,
    TransportProviderId,
};

use super::model::{YtDlpCandidateComponentRole, YtDlpNormalizedCandidate};
use super::request_material::YtDlpRequestMaterialViolation;

/// Bounded redirect budget public CDN resource-а.
const PUBLIC_MEDIA_REDIRECT_HOPS: u8 = 8;

/// Runtime-owned values, назначаемые composition root-ом для одного open attempt-а.
#[derive(Clone)]
pub struct YtDlpTransportRequestContext {
    /// Exact concrete provider из process-local registry.
    provider: TransportProviderId,
    /// Runtime generation, независимая от extraction generation.
    source_generation: SourceGeneration,
    /// Общий cooperative cancellation token attempt-а.
    cancellation: CancellationToken,
}

impl YtDlpTransportRequestContext {
    /// Собирает named context без позиционных runtime literals в mapping-е.
    #[must_use]
    pub fn new(
        provider: TransportProviderId,
        source_generation: SourceGeneration,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            provider,
            source_generation,
            cancellation,
        }
    }
}

/// Один реальный candidate component вместе с demux container hint-ом.
#[derive(Debug)]
pub struct YtDlpTransportComponent {
    /// Роль ресурса нужна composition root-у для separate A/V.
    role: MediaComponentRole,
    /// Нормализованный container family для demux registry.
    container: ContainerFamily,
    /// Полный neutral open request.
    request: TransportOpenRequest,
}

impl YtDlpTransportComponent {
    /// Возвращает semantic component role.
    #[must_use]
    pub const fn role(&self) -> MediaComponentRole {
        self.role
    }

    /// Возвращает normalized container hint.
    #[must_use]
    pub const fn container(&self) -> ContainerFamily {
        self.container
    }

    /// Передаёт owned request concrete transport registry.
    #[must_use]
    pub fn into_request(self) -> TransportOpenRequest {
        self.request
    }
}

/// Ошибка построения neutral transport request до provider side effects.
#[derive(Debug, Error)]
pub enum YtDlpTransportRequestError {
    /// Request material не принадлежит реализованному progressive/public subset-у.
    #[error("YtDlp request material нельзя открыть progressive HTTP transport-ом")]
    RequestMaterial(#[source] YtDlpRequestMaterialViolation),
    /// Extractor вернул syntactically invalid либо non-HTTP target.
    #[error("YtDlp component содержит недопустимый HTTP target")]
    Target(#[source] source_core::HttpRequestTargetError),
    /// Candidate exact/semantic identities нарушили source lineage.
    #[error("YtDlp component identity нарушает source lineage")]
    Identity(#[source] MediaComponentIdentityError),
    /// Internal redirect constant вышел за transport API budget.
    #[error("YtDlp redirect policy нарушает transport contract")]
    RedirectLimit(#[source] RedirectHopLimitError),
    /// Neutral request rejected scoped-material contract.
    #[error("YtDlp transport request нарушает secret scope contract")]
    Request(#[source] TransportOpenRequestError),
    /// Descriptor layout не совпал с service-owned component roles.
    #[error("YtDlp component roles не совпадают с descriptor layout")]
    LayoutMismatch,
}

impl YtDlpNormalizedCandidate {
    /// Строит один request для single candidate либо два для exact compound merge.
    pub fn transport_components(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<Vec<YtDlpTransportComponent>, YtDlpTransportRequestError> {
        let mut components = Vec::with_capacity(self.component_requests.len());
        for component in &self.component_requests {
            let role = media_component_role(component.role);
            let container = component_container(self.descriptor().layout(), component.role)
                .ok_or(YtDlpTransportRequestError::LayoutMismatch)?;
            let target = HttpRequestTarget::parse_exact(
                component
                    .material
                    .progressive_http_target()
                    .map_err(YtDlpTransportRequestError::RequestMaterial)?,
            )
            .map_err(YtDlpTransportRequestError::Target)?;
            let identity = MediaComponentIdentity::new(
                self.descriptor().identity().clone(),
                self.descriptor().semantic_identity().clone(),
                role,
            )
            .map_err(YtDlpTransportRequestError::Identity)?;
            let root_path =
                HttpPathScope::new("/").expect("static root HTTP path scope must remain valid");
            let secrets =
                SecretRequestContext::builder(SecretRequestScope::from_target(&target, root_path))
                    .build();
            let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
                .map_err(YtDlpTransportRequestError::RedirectLimit)?;
            let request = TransportOpenRequest::new(
                context.provider.clone(),
                identity,
                target,
                MediaPresentation::Vod,
                context.source_generation,
                secrets,
                RedirectPolicy::cross_origin_without_secrets(redirect_limit),
                context.cancellation.clone(),
            )
            .map_err(YtDlpTransportRequestError::Request)?;
            components.push(YtDlpTransportComponent {
                role,
                container,
                request,
            });
        }
        Ok(components)
    }
}

/// Маппит service role в transport vocabulary без ordinal semantics.
const fn media_component_role(role: YtDlpCandidateComponentRole) -> MediaComponentRole {
    match role {
        YtDlpCandidateComponentRole::Muxed => MediaComponentRole::Muxed,
        YtDlpCandidateComponentRole::Video => MediaComponentRole::Video,
        YtDlpCandidateComponentRole::Audio => MediaComponentRole::Audio,
    }
}

/// Берёт container ровно того descriptor component-а, которому принадлежит material.
fn component_container(
    layout: &StreamLayout,
    role: YtDlpCandidateComponentRole,
) -> Option<ContainerFamily> {
    match (layout, role) {
        (StreamLayout::Muxed(component), YtDlpCandidateComponentRole::Muxed) => {
            component.container().consistent_family().ok().flatten()
        }
        (StreamLayout::Separate { video, .. }, YtDlpCandidateComponentRole::Video)
        | (StreamLayout::VideoOnly(video), YtDlpCandidateComponentRole::Video) => {
            video.container().consistent_family().ok().flatten()
        }
        (StreamLayout::Separate { audio, .. }, YtDlpCandidateComponentRole::Audio)
        | (StreamLayout::AudioOnly(audio), YtDlpCandidateComponentRole::Audio) => {
            audio.container().consistent_family().ok().flatten()
        }
        _ => None,
    }
}
