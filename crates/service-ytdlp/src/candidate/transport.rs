//! Provider-neutral mapping S19 request material-а в S21T open requests.

use source_core::{
    CancellationToken, HttpHeader, HttpHeaderValidationError, HttpPathScope, HttpRequestTarget,
    ValidatedHttpHeaders,
};
use thiserror::Error;
use url::Url;
use web_media_core::{CodecFamily, CodecKind, ContainerFamily, StreamLayout, TransportFamily};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentIdentityError, MediaComponentRole, MediaPresentation,
    RedirectHopLimit, RedirectHopLimitError, RedirectPolicy, SecretQueryOverrideError,
    SecretRequestContext, SecretRequestScope, SourceGeneration, TransportOpenRequest,
    TransportOpenRequestError, TransportProviderId,
};

use super::model::{
    YtDlpCandidateComponentRequest, YtDlpCandidateComponentRole, YtDlpNormalizedCandidate,
};
use super::request_material::{
    YtDlpDashFragmentLocatorKind, YtDlpDashInputKind, YtDlpDashRequestMaterial,
    YtDlpDashRequestMaterialViolation, YtDlpHlsRequestMaterial, YtDlpHlsRequestMaterialViolation,
    YtDlpRequestMaterialViolation, YtDlpSmoothManifestRequestMaterial,
    YtDlpSmoothManifestRequestMaterialViolation,
};

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

/// DASH component вместе с borrowed validated material и neutral S21T request.
#[derive(Debug)]
pub struct YtDlpDashTransportComponent<'candidate> {
    /// Explicit muxed/video/audio role.
    role: MediaComponentRole,
    /// Proven normalized container.
    container: ContainerFamily,
    /// Borrowed service-owned MPD/fragment semantics.
    material: YtDlpDashRequestMaterial<'candidate>,
    /// Owned secret-scoped request для concrete HTTP provider-а.
    request: TransportOpenRequest,
}

impl<'candidate> YtDlpDashTransportComponent<'candidate> {
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

    /// Возвращает validated borrowed DASH material.
    #[must_use]
    pub const fn material(&self) -> &YtDlpDashRequestMaterial<'candidate> {
        &self.material
    }

    /// Передаёт owned request и сохраняет borrowed material рядом.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        MediaComponentRole,
        ContainerFamily,
        YtDlpDashRequestMaterial<'candidate>,
        TransportOpenRequest,
    ) {
        (self.role, self.container, self.material, self.request)
    }
}

/// Ошибка построения neutral transport request до provider side effects.
#[derive(Debug, Error)]
pub enum YtDlpTransportRequestError {
    /// Request material не принадлежит реализованному progressive/public subset-у.
    #[error("YtDlp request material нельзя открыть progressive HTTP transport-ом")]
    RequestMaterial(#[source] YtDlpRequestMaterialViolation),
    /// Request material не является single-resource HLS profile.
    #[error("YtDlp request material нельзя выразить как HLS open request")]
    HlsRequestMaterial(#[source] YtDlpHlsRequestMaterialViolation),
    /// Request material не соответствует static DASH profile.
    #[error("YtDlp request material нельзя выразить как DASH open request")]
    DashRequestMaterial(#[source] YtDlpDashRequestMaterialViolation),
    /// Request material не соответствует manifest-only Smooth Streaming profile.
    #[error("YtDlp request material нельзя выразить как Smooth manifest request")]
    SmoothRequestMaterial(#[source] YtDlpSmoothManifestRequestMaterialViolation),
    /// Scoped segment/key query не прошли shared secret policy.
    #[error("YtDlp HLS query override нарушает secret scope contract")]
    HlsQueryProjection(#[source] SecretQueryOverrideError),
    /// Scoped DASH segment query не прошёл shared secret policy.
    #[error("YtDlp DASH query override нарушает secret scope contract")]
    DashQueryProjection(#[source] SecretQueryOverrideError),
    /// Validated relative fragment не разрешился в absolute transport target.
    #[error("YtDlp DASH fragment target нельзя разрешить безопасно")]
    DashTargetResolution,
    /// Extractor вернул syntactically invalid либо non-HTTP target.
    #[error("YtDlp component содержит недопустимый HTTP target")]
    Target(#[source] source_core::HttpRequestTargetError),
    /// Smooth manifest target нарушил neutral HTTP target contract после material proof.
    #[error("YtDlp Smooth manifest содержит недопустимый HTTP target")]
    SmoothTarget(#[source] source_core::HttpRequestTargetError),
    /// Candidate exact/semantic identities нарушили source lineage.
    #[error("YtDlp component identity нарушает source lineage")]
    Identity(#[source] MediaComponentIdentityError),
    /// Internal redirect constant вышел за transport API budget.
    #[error("YtDlp redirect policy нарушает transport contract")]
    RedirectLimit(#[source] RedirectHopLimitError),
    /// Neutral request rejected scoped-material contract.
    #[error("YtDlp transport request нарушает secret scope contract")]
    Request(#[source] TransportOpenRequestError),
    /// Extractor headers/cookies нельзя безопасно сериализовать как HTTP fields.
    #[error("YtDlp HTTP authorization material имеет недопустимую serialization")]
    AuthorizationSerialization(#[source] HttpHeaderValidationError),
    /// Descriptor layout не совпал с service-owned component roles.
    #[error("YtDlp component roles не совпадают с descriptor layout")]
    LayoutMismatch,
    /// Initial HLS profile принимает один selected manifest resource.
    #[error("YtDlp HLS candidate должен содержать ровно один manifest component")]
    HlsComponentCount,
    /// Smooth VOD candidate обязан содержать ровно один request component.
    #[error("YtDlp Smooth candidate должен содержать ровно один manifest component")]
    SmoothComponentCount,
    /// Единственный Smooth component обязан иметь semantic role `Muxed`.
    #[error("YtDlp Smooth component должен иметь muxed role")]
    SmoothComponentRole,
    /// Smooth VOD projection принимает только muxed descriptor layout.
    #[error("YtDlp Smooth candidate должен иметь muxed layout")]
    SmoothLayout,
    /// Descriptor transport обязан быть exact Smooth Streaming.
    #[error("YtDlp Smooth candidate использует другую transport family")]
    SmoothTransport,
    /// Approved Smooth row обязан описывать fragmented ISO BMFF container.
    #[error("YtDlp Smooth candidate использует неподдерживаемый container")]
    SmoothContainer,
    /// Approved Smooth row обязан содержать H.264 video.
    #[error("YtDlp Smooth candidate использует неподдерживаемый video codec")]
    SmoothVideoCodec,
    /// Approved Smooth row обязан содержать AAC audio.
    #[error("YtDlp Smooth candidate использует неподдерживаемый audio codec")]
    SmoothAudioCodec,
}

impl YtDlpNormalizedCandidate {
    /// Проецирует каждый selected DASH component в neutral request без concrete runtime deps.
    pub fn dash_transport_components(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<Vec<YtDlpDashTransportComponent<'_>>, YtDlpTransportRequestError> {
        let mut projected = Vec::with_capacity(self.component_requests.len());
        for component in &self.component_requests {
            let role = media_component_role(component.role);
            let container = component_container(self.descriptor().layout(), component.role)
                .ok_or(YtDlpTransportRequestError::LayoutMismatch)?;
            let material = component
                .material
                .dash_request_material()
                .map_err(YtDlpTransportRequestError::DashRequestMaterial)?;
            let anchor = dash_transport_anchor(&material)?;
            let target = HttpRequestTarget::parse_exact(&anchor)
                .map_err(YtDlpTransportRequestError::Target)?;
            let identity = MediaComponentIdentity::new(
                self.descriptor().identity().clone(),
                self.descriptor().semantic_identity().clone(),
                role,
            )
            .map_err(YtDlpTransportRequestError::Identity)?;
            let path_scope = dash_resource_path_scope(&material, &target)?;
            let secret_scope = SecretRequestScope::from_target(&target, path_scope);
            let serialized_headers = material
                .request_context()
                .headers()
                .map(|(name, value)| HttpHeader::new(name, value))
                .collect::<Vec<_>>();
            let serialized_headers = ValidatedHttpHeaders::new(serialized_headers)
                .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
            let mut secret_builder =
                SecretRequestContext::builder(secret_scope).with_headers(serialized_headers);
            if let Some(serialized_cookies) = material.request_context().serialized_cookies() {
                secret_builder = secret_builder
                    .with_serialized_cookies(serialized_cookies)
                    .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
            }
            secret_builder = material
                .project_scoped_query(secret_builder)
                .map_err(YtDlpTransportRequestError::DashQueryProjection)?;
            let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
                .map_err(YtDlpTransportRequestError::RedirectLimit)?;
            let request = TransportOpenRequest::new(
                context.provider.clone(),
                identity,
                target,
                MediaPresentation::Vod,
                context.source_generation,
                secret_builder.build(),
                RedirectPolicy::cross_origin_without_secrets(redirect_limit),
                context.cancellation.clone(),
            )
            .map_err(YtDlpTransportRequestError::Request)?;
            projected.push(YtDlpDashTransportComponent {
                role,
                container,
                material,
                request,
            });
        }
        Ok(projected)
    }

    /// Возвращает validated borrowed HLS material единственного manifest component-а.
    pub fn hls_request_material(
        &self,
    ) -> Result<YtDlpHlsRequestMaterial<'_>, YtDlpTransportRequestError> {
        let [component] = self.component_requests.as_ref() else {
            return Err(YtDlpTransportRequestError::HlsComponentCount);
        };
        component
            .material
            .hls_request_material()
            .map_err(YtDlpTransportRequestError::HlsRequestMaterial)
    }

    /// Проецирует HLS headers/cookies/segment+key queries в один neutral request.
    pub fn hls_transport_request(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<TransportOpenRequest, YtDlpTransportRequestError> {
        let [component] = self.component_requests.as_ref() else {
            return Err(YtDlpTransportRequestError::HlsComponentCount);
        };
        let hls_material = component
            .material
            .hls_request_material()
            .map_err(YtDlpTransportRequestError::HlsRequestMaterial)?;
        let authorization_material = component
            .material
            .http_authorization_material()
            .map_err(YtDlpTransportRequestError::RequestMaterial)?;
        let target =
            HttpRequestTarget::parse_exact(hls_material.manifest().selected_url_for_resolution())
                .map_err(YtDlpTransportRequestError::Target)?;
        let role = media_component_role(component.role);
        let identity = MediaComponentIdentity::new(
            self.descriptor().identity().clone(),
            self.descriptor().semantic_identity().clone(),
            role,
        )
        .map_err(YtDlpTransportRequestError::Identity)?;
        let path_scope = HttpPathScope::from_target_path(&target);
        let secret_scope = SecretRequestScope::from_target(&target, path_scope);
        let serialized_headers = authorization_material
            .headers()
            .map(|(name, value)| HttpHeader::new(name, value))
            .collect::<Vec<_>>();
        let serialized_headers = ValidatedHttpHeaders::new(serialized_headers)
            .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
        let mut secret_builder =
            SecretRequestContext::builder(secret_scope).with_headers(serialized_headers);
        if let Some(serialized_cookies) = authorization_material.serialized_cookies() {
            secret_builder = secret_builder
                .with_serialized_cookies(serialized_cookies)
                .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
        }
        secret_builder = hls_material
            .project_scoped_queries(secret_builder)
            .map_err(YtDlpTransportRequestError::HlsQueryProjection)?;
        let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
            .map_err(YtDlpTransportRequestError::RedirectLimit)?;
        TransportOpenRequest::new(
            context.provider.clone(),
            identity,
            target,
            MediaPresentation::Vod,
            context.source_generation,
            secret_builder.build(),
            RedirectPolicy::cross_origin_without_secrets(redirect_limit),
            context.cancellation.clone(),
        )
        .map_err(YtDlpTransportRequestError::Request)
    }

    /// Строит один secret-scoped VOD request для exact muxed H.264+AAC ISM manifest-а.
    pub fn smooth_manifest_transport_request(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<TransportOpenRequest, YtDlpTransportRequestError> {
        let component = smooth_manifest_component(self)?;
        let material = component
            .material
            .smooth_manifest_request_material()
            .map_err(YtDlpTransportRequestError::SmoothRequestMaterial)?;
        let target = HttpRequestTarget::parse_exact(material.manifest_target_for_fetch())
            .map_err(YtDlpTransportRequestError::SmoothTarget)?;
        let identity = MediaComponentIdentity::new(
            self.descriptor().identity().clone(),
            self.descriptor().semantic_identity().clone(),
            MediaComponentRole::Muxed,
        )
        .map_err(YtDlpTransportRequestError::Identity)?;
        let secrets = smooth_manifest_secret_context(&material, &target)?;
        let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
            .map_err(YtDlpTransportRequestError::RedirectLimit)?;

        TransportOpenRequest::new(
            context.provider.clone(),
            identity,
            target,
            MediaPresentation::Vod,
            context.source_generation,
            secrets,
            RedirectPolicy::cross_origin_without_secrets(redirect_limit),
            context.cancellation.clone(),
        )
        .map_err(YtDlpTransportRequestError::Request)
    }

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
            let request_material = component
                .material
                .progressive_http_request_material()
                .map_err(YtDlpTransportRequestError::RequestMaterial)?;
            let target = HttpRequestTarget::parse_exact(request_material.target())
                .map_err(YtDlpTransportRequestError::Target)?;
            let identity = MediaComponentIdentity::new(
                self.descriptor().identity().clone(),
                self.descriptor().semantic_identity().clone(),
                role,
            )
            .map_err(YtDlpTransportRequestError::Identity)?;
            let path_scope = HttpPathScope::from_target_path(&target);
            let secret_scope = SecretRequestScope::from_target(&target, path_scope);
            let serialized_headers = request_material
                .headers()
                .map(|(name, value)| HttpHeader::new(name, value))
                .collect::<Vec<_>>();
            let serialized_headers = ValidatedHttpHeaders::new(serialized_headers)
                .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
            let mut secret_builder =
                SecretRequestContext::builder(secret_scope).with_headers(serialized_headers);
            if let Some(serialized_cookies) = request_material.serialized_cookies() {
                secret_builder = secret_builder
                    .with_serialized_cookies(serialized_cookies)
                    .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
            }
            let secrets = secret_builder.build();
            let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
                .map_err(YtDlpTransportRequestError::RedirectLimit)?;
            let mut request = TransportOpenRequest::new(
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
            if let Some(http_range_request_limit) = request_material.http_range_request_limit() {
                request = request.with_http_range_request_limit(http_range_request_limit);
            }
            components.push(YtDlpTransportComponent {
                role,
                container,
                request,
            });
        }
        Ok(components)
    }
}

/// Доказывает exact candidate-level Smooth Streaming profile до material projection.
fn smooth_manifest_component(
    candidate: &YtDlpNormalizedCandidate,
) -> Result<&YtDlpCandidateComponentRequest, YtDlpTransportRequestError> {
    let [component] = candidate.component_requests.as_ref() else {
        return Err(YtDlpTransportRequestError::SmoothComponentCount);
    };
    let StreamLayout::Muxed(muxed) = candidate.descriptor().layout() else {
        return Err(YtDlpTransportRequestError::SmoothLayout);
    };
    if component.role != YtDlpCandidateComponentRole::Muxed {
        return Err(YtDlpTransportRequestError::SmoothComponentRole);
    }
    if muxed.transport().family() != TransportFamily::SmoothStreaming {
        return Err(YtDlpTransportRequestError::SmoothTransport);
    }
    if muxed.container().consistent_family().ok() != Some(Some(ContainerFamily::FragmentedIsoBmff))
    {
        return Err(YtDlpTransportRequestError::SmoothContainer);
    }
    if muxed.video().codec().kind() != CodecKind::Known(CodecFamily::H264) {
        return Err(YtDlpTransportRequestError::SmoothVideoCodec);
    }
    if muxed.audio().codec().kind() != CodecKind::Known(CodecFamily::Aac) {
        return Err(YtDlpTransportRequestError::SmoothAudioCodec);
    }
    Ok(component)
}

/// Собирает S26-compatible ephemeral headers/cookies для одного manifest source-а.
fn smooth_manifest_secret_context(
    material: &YtDlpSmoothManifestRequestMaterial<'_>,
    target: &HttpRequestTarget,
) -> Result<SecretRequestContext, YtDlpTransportRequestError> {
    let path_scope = HttpPathScope::from_target_path(target);
    let secret_scope = SecretRequestScope::from_target(target, path_scope);
    let serialized_headers = material
        .headers()
        .map(|(name, value)| HttpHeader::new(name, value))
        .collect::<Vec<_>>();
    let serialized_headers = ValidatedHttpHeaders::new(serialized_headers)
        .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
    let mut secret_builder =
        SecretRequestContext::builder(secret_scope).with_headers(serialized_headers);
    if let Some(serialized_cookies) = material.serialized_cookies() {
        secret_builder = secret_builder
            .with_serialized_cookies(serialized_cookies)
            .map_err(YtDlpTransportRequestError::AuthorizationSerialization)?;
    }
    Ok(secret_builder.build())
}

/// Выбирает request-scope anchor без fallback между authoritative inputs.
fn dash_transport_anchor(
    material: &YtDlpDashRequestMaterial<'_>,
) -> Result<String, YtDlpTransportRequestError> {
    match material.input().kind() {
        YtDlpDashInputKind::Manifest => material
            .input()
            .manifest_url_for_fetch()
            .map(ToOwned::to_owned)
            .ok_or(YtDlpTransportRequestError::DashTargetResolution),
        YtDlpDashInputKind::SerializedFragments => {
            let fragment = material
                .input()
                .fragments()
                .next()
                .ok_or(YtDlpTransportRequestError::DashTargetResolution)?;
            match fragment.locator_kind() {
                YtDlpDashFragmentLocatorKind::AbsoluteUrl => {
                    Ok(fragment.locator_for_transport().to_owned())
                }
                YtDlpDashFragmentLocatorKind::RelativePath => {
                    let base = fragment
                        .base_url_for_relative_resolution()
                        .ok_or(YtDlpTransportRequestError::DashTargetResolution)?;
                    let parsed_base = Url::parse(base)
                        .map_err(|_| YtDlpTransportRequestError::DashTargetResolution)?;
                    parsed_base
                        .join(fragment.locator_for_transport())
                        .map(|resolved| resolved.into())
                        .map_err(|_| YtDlpTransportRequestError::DashTargetResolution)
                }
            }
        }
    }
}

/// Ограничивает DASH credentials директорией authoritative MPD/fragment base-а.
///
/// Exact-file scope progressive source-а недостаточен segmented transport-у:
/// sibling init/media resources должны получить тот же fresh request context.
/// Origin и HTTPS downgrade по-прежнему проверяет shared S21T boundary.
fn dash_resource_path_scope(
    material: &YtDlpDashRequestMaterial<'_>,
    anchor: &HttpRequestTarget,
) -> Result<HttpPathScope, YtDlpTransportRequestError> {
    let scope_locator = if material.input().kind() == YtDlpDashInputKind::SerializedFragments {
        material
            .input()
            .fragments()
            .next()
            .and_then(|fragment| {
                fragment
                    .base_url_for_relative_resolution()
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| anchor.expose_secret_for_request().to_owned())
    } else {
        anchor.expose_secret_for_request().to_owned()
    };
    let parsed_scope =
        Url::parse(&scope_locator).map_err(|_| YtDlpTransportRequestError::DashTargetResolution)?;
    let scope_path = parsed_scope.path();
    let directory_path = if scope_path.ends_with('/') {
        scope_path.to_owned()
    } else {
        let parent = scope_path
            .rsplit_once('/')
            .map_or("/", |(parent, _)| parent);
        format!("{parent}/")
    };
    HttpPathScope::new(directory_path).map_err(|_| YtDlpTransportRequestError::DashTargetResolution)
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
