//! Provider-neutral mapping S19 request material-а в S21T open requests.

mod http_secret;
mod smooth;

use source_core::{
    CancellationToken, FtpRequestTarget, HttpHeaderValidationError, HttpPathScope,
    HttpRequestTarget,
};
use thiserror::Error;
use web_media_core::{ContainerFamily, StreamLayout, TransportFamily};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentIdentityError, MediaComponentRole, MediaPresentation,
    RedirectHopLimit, RedirectHopLimitError, RedirectPolicy, SecretQueryOverrideError,
    SecretRequestScope, SourceGeneration, TransportOpenRequest, TransportOpenRequestError,
    TransportProviderId,
};

use self::http_secret::{
    dash_resource_path_scope, dash_transport_anchor, hds_resource_path_scope,
    http_secret_context_builder, resource_directory_path_scope,
};

use super::model::{
    YtDlpCandidateComponentRequest, YtDlpCandidateComponentRole, YtDlpNormalizedCandidate,
};
use super::request_material::{
    YtDlpDashRequestMaterial, YtDlpDashRequestMaterialViolation,
    YtDlpHdsManifestRequestMaterialViolation, YtDlpHlsRequestMaterial,
    YtDlpHlsRequestMaterialViolation, YtDlpRequestMaterialViolation,
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

/// Provider set для component-wise progressive HTTP/FTP composition.
#[derive(Clone)]
pub struct YtDlpProgressiveTransportRequestContext {
    /// HTTP provider context того же attempt/generation.
    http: YtDlpTransportRequestContext,
    /// FTP provider context того же attempt/generation.
    ftp: YtDlpTransportRequestContext,
}

impl YtDlpProgressiveTransportRequestContext {
    /// Собирает оба provider context-а с общими lifecycle fences.
    #[must_use]
    pub fn new(
        http_provider: TransportProviderId,
        ftp_provider: TransportProviderId,
        source_generation: SourceGeneration,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            http: YtDlpTransportRequestContext::new(
                http_provider,
                source_generation,
                cancellation.clone(),
            ),
            ftp: YtDlpTransportRequestContext::new(ftp_provider, source_generation, cancellation),
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
    /// Request material не принадлежит progressive FTP subset-у.
    #[error("YtDlp request material нельзя открыть progressive FTP transport-ом")]
    FtpRequestMaterial(#[source] YtDlpRequestMaterialViolation),
    /// Request material не является single-resource HLS profile.
    #[error("YtDlp request material нельзя выразить как HLS open request")]
    HlsRequestMaterial(#[source] YtDlpHlsRequestMaterialViolation),
    /// Request material не соответствует static DASH profile.
    #[error("YtDlp request material нельзя выразить как DASH open request")]
    DashRequestMaterial(#[source] YtDlpDashRequestMaterialViolation),
    /// Request material не соответствует static HDS F4M/F4F VOD profile.
    #[error("YtDlp request material нельзя выразить как HDS manifest request")]
    HdsRequestMaterial(#[source] YtDlpHdsManifestRequestMaterialViolation),
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
    /// Extractor вернул syntactically invalid либо non-FTP target.
    #[error("YtDlp component содержит недопустимый FTP target")]
    FtpTarget(#[source] source_core::FtpRequestTargetError),
    /// Smooth manifest target нарушил neutral HTTP target contract после material proof.
    #[error("YtDlp Smooth manifest содержит недопустимый HTTP target")]
    SmoothTarget(#[source] source_core::HttpRequestTargetError),
    /// Smooth child resources cannot receive a safe presentation-directory path scope.
    #[error("YtDlp Smooth manifest target cannot create a presentation resource path scope")]
    SmoothTargetResolution,
    /// HDS manifest target нарушил neutral HTTP target contract.
    #[error("YtDlp HDS manifest содержит недопустимый HTTP target")]
    HdsTarget(#[source] source_core::HttpRequestTargetError),
    /// HDS child/media resources cannot receive a safe path scope.
    #[error("YtDlp HDS manifest target cannot create a resource path scope")]
    HdsTargetResolution,
    /// HLS media/key siblings cannot receive a safe playlist-directory path scope.
    #[error("YtDlp HLS playlist target cannot create a resource path scope")]
    HlsTargetResolution,
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
    /// Progressive adapter не принимает manifest/segment transport family.
    #[error("YtDlp component использует non-progressive transport family")]
    NonProgressiveTransportFamily,
    /// Initial HLS profile принимает один selected manifest resource.
    #[error("YtDlp HLS candidate должен содержать ровно один manifest component")]
    HlsComponentCount,
    /// Smooth VOD candidate обязан содержать один muxed либо exact video+audio request shape.
    #[error("YtDlp Smooth candidate имеет неподдерживаемое число request components")]
    SmoothComponentCount,
    /// Smooth request roles обязаны совпадать с muxed либо separate descriptor layout.
    #[error("YtDlp Smooth component roles не совпадают с descriptor layout")]
    SmoothComponentRole,
    /// Smooth VOD projection принимает muxed либо separate video+audio descriptor layout.
    #[error("YtDlp Smooth candidate должен иметь muxed либо separate A/V layout")]
    SmoothLayout,
    /// Separate Smooth components должны ссылаться на один byte-exact presentation Manifest.
    #[error("YtDlp Smooth A/V components ссылаются на разные presentation manifests")]
    SmoothPresentationTargetMismatch,
    /// Separate Smooth components должны иметь один effective HTTP authorization context.
    #[error("YtDlp Smooth A/V components имеют разные presentation request contexts")]
    SmoothPresentationRequestContextMismatch,
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
    /// S38 HDS base profile accepts one provider-probed manifest component.
    #[error("YtDlp HDS candidate должен содержать ровно один content-probed component")]
    HdsComponentShape,
    /// HDS component role must preserve the provider-probed resource contract.
    #[error("YtDlp HDS component должен иметь content-probed role")]
    HdsComponentRole,
    /// Descriptor transport must be HDS.
    #[error("YtDlp HDS candidate использует другую transport family")]
    HdsTransport,
    /// S30 F4F container is required.
    #[error("YtDlp HDS candidate использует неподдерживаемый container")]
    HdsContainer,
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
            let mut secret_builder = http_secret_context_builder(
                secret_scope,
                material.request_context().headers(),
                material.request_context().cookies(),
            )?;
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
        // Sibling media/key URLs живут в каталоге плейлиста, не в path самого index.m3u8.
        let path_scope = resource_directory_path_scope(&target)
            .ok_or(YtDlpTransportRequestError::HlsTargetResolution)?;
        let secret_scope = SecretRequestScope::from_target(&target, path_scope);
        let mut secret_builder = http_secret_context_builder(
            secret_scope,
            authorization_material.headers(),
            authorization_material.cookies(),
        )?;
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

    /// Строит один secret-scoped VOD request для F4M hierarchy root-а.
    pub fn hds_transport_request(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<TransportOpenRequest, YtDlpTransportRequestError> {
        let component = hds_manifest_component(self)?;
        let material = component
            .material
            .hds_manifest_request_material()
            .map_err(YtDlpTransportRequestError::HdsRequestMaterial)?;
        let target = HttpRequestTarget::parse_exact(material.manifest_target_for_fetch())
            .map_err(YtDlpTransportRequestError::HdsTarget)?;
        let identity = MediaComponentIdentity::new(
            self.descriptor().identity().clone(),
            self.descriptor().semantic_identity().clone(),
            media_component_role(component.role),
        )
        .map_err(YtDlpTransportRequestError::Identity)?;
        let path_scope = hds_resource_path_scope(&target)?;
        let secret_scope = SecretRequestScope::from_target(&target, path_scope);
        let secret_builder =
            http_secret_context_builder(secret_scope, material.headers(), material.cookies())?;
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

    /// Строит HTTP request-ы для single candidate либо exact compound merge.
    pub fn transport_components(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<Vec<YtDlpTransportComponent>, YtDlpTransportRequestError> {
        self.component_requests
            .iter()
            .map(|component| build_http_transport_component(self, component, context))
            .collect()
    }

    /// Строит FTP request-ы для single candidate либо exact compound merge.
    pub fn ftp_transport_components(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<Vec<YtDlpTransportComponent>, YtDlpTransportRequestError> {
        self.component_requests
            .iter()
            .map(|component| build_ftp_transport_component(self, component, context))
            .collect()
    }

    /// Строит каждый progressive component через provider его exact transport family.
    pub fn progressive_transport_components(
        &self,
        context: &YtDlpProgressiveTransportRequestContext,
    ) -> Result<Vec<YtDlpTransportComponent>, YtDlpTransportRequestError> {
        self.component_requests
            .iter()
            .map(|component| {
                let family = component_transport_family(self.descriptor().layout(), component.role)
                    .ok_or(YtDlpTransportRequestError::LayoutMismatch)?;
                match family {
                    TransportFamily::ProgressiveHttp(_) => {
                        build_http_transport_component(self, component, &context.http)
                    }
                    TransportFamily::ProgressiveFtp(_) => {
                        build_ftp_transport_component(self, component, &context.ftp)
                    }
                    _ => Err(YtDlpTransportRequestError::NonProgressiveTransportFamily),
                }
            })
            .collect()
    }
}

/// Строит один HTTP component, сохраняя scoped auth и redirect policy.
fn build_http_transport_component(
    candidate: &YtDlpNormalizedCandidate,
    component: &YtDlpCandidateComponentRequest,
    context: &YtDlpTransportRequestContext,
) -> Result<YtDlpTransportComponent, YtDlpTransportRequestError> {
    let role = media_component_role(component.role);
    let container = component_container(candidate.descriptor().layout(), component.role)
        .ok_or(YtDlpTransportRequestError::LayoutMismatch)?;
    let request_material = component
        .material
        .progressive_http_request_material()
        .map_err(YtDlpTransportRequestError::RequestMaterial)?;
    let target = HttpRequestTarget::parse_exact(request_material.target())
        .map_err(YtDlpTransportRequestError::Target)?;
    let identity = component_identity(candidate, role)?;
    let path_scope = HttpPathScope::from_target_path(&target);
    let secret_scope = SecretRequestScope::from_target(&target, path_scope);
    let secret_builder = http_secret_context_builder(
        secret_scope,
        request_material.headers(),
        request_material.cookies(),
    )?;
    let redirect_limit = RedirectHopLimit::new(PUBLIC_MEDIA_REDIRECT_HOPS)
        .map_err(YtDlpTransportRequestError::RedirectLimit)?;
    let mut request = TransportOpenRequest::new(
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
    if let Some(http_range_request_limit) = request_material.http_range_request_limit() {
        request = request.with_http_range_request_limit(http_range_request_limit);
    }
    Ok(YtDlpTransportComponent {
        role,
        container,
        request,
    })
}

/// Строит один FTP component без HTTP-only material.
fn build_ftp_transport_component(
    candidate: &YtDlpNormalizedCandidate,
    component: &YtDlpCandidateComponentRequest,
    context: &YtDlpTransportRequestContext,
) -> Result<YtDlpTransportComponent, YtDlpTransportRequestError> {
    let role = media_component_role(component.role);
    let container = component_container(candidate.descriptor().layout(), component.role)
        .ok_or(YtDlpTransportRequestError::LayoutMismatch)?;
    let request_material = component
        .material
        .progressive_ftp_request_material()
        .map_err(YtDlpTransportRequestError::FtpRequestMaterial)?;
    let target = FtpRequestTarget::parse_exact(request_material.target())
        .map_err(YtDlpTransportRequestError::FtpTarget)?;
    let request = TransportOpenRequest::for_ftp(
        context.provider.clone(),
        component_identity(candidate, role)?,
        target,
        MediaPresentation::Vod,
        context.source_generation,
        context.cancellation.clone(),
    )
    .map_err(YtDlpTransportRequestError::Request)?;
    Ok(YtDlpTransportComponent {
        role,
        container,
        request,
    })
}

/// Собирает identity одного physical resource без provider-specific knowledge.
fn component_identity(
    candidate: &YtDlpNormalizedCandidate,
    role: MediaComponentRole,
) -> Result<MediaComponentIdentity, YtDlpTransportRequestError> {
    MediaComponentIdentity::new(
        candidate.descriptor().identity().clone(),
        candidate.descriptor().semantic_identity().clone(),
        role,
    )
    .map_err(YtDlpTransportRequestError::Identity)
}

/// Возвращает exact transport family physical component-а layout-а.
fn component_transport_family(
    layout: &StreamLayout,
    role: YtDlpCandidateComponentRole,
) -> Option<TransportFamily> {
    match (layout, role) {
        (StreamLayout::Muxed(component), YtDlpCandidateComponentRole::Muxed) => {
            Some(component.transport().family())
        }
        (StreamLayout::HlsMuxedCodecDeferred(component), YtDlpCandidateComponentRole::Muxed) => {
            Some(component.transport().family())
        }
        (StreamLayout::ContentProbed(component), YtDlpCandidateComponentRole::ContentProbed) => {
            Some(component.transport().family())
        }
        (StreamLayout::VideoOnly(component), YtDlpCandidateComponentRole::Video) => {
            Some(component.transport().family())
        }
        (StreamLayout::AudioOnly(component), YtDlpCandidateComponentRole::Audio) => {
            Some(component.transport().family())
        }
        (StreamLayout::Separate { video, .. }, YtDlpCandidateComponentRole::Video) => {
            Some(video.transport().family())
        }
        (StreamLayout::Separate { audio, .. }, YtDlpCandidateComponentRole::Audio) => {
            Some(audio.transport().family())
        }
        _ => None,
    }
}

/// Доказывает exact single-component HDS F4M/F4F VOD profile.
fn hds_manifest_component(
    candidate: &YtDlpNormalizedCandidate,
) -> Result<&YtDlpCandidateComponentRequest, YtDlpTransportRequestError> {
    let [component] = candidate.component_requests.as_ref() else {
        return Err(YtDlpTransportRequestError::HdsComponentShape);
    };
    let (transport, probe_container) = match candidate.descriptor().layout() {
        StreamLayout::ContentProbed(probed) => {
            (probed.transport().family(), probed.probe_container())
        }
        _ => return Err(YtDlpTransportRequestError::HdsComponentShape),
    };
    if component.role != YtDlpCandidateComponentRole::ContentProbed {
        return Err(YtDlpTransportRequestError::HdsComponentRole);
    }
    if transport != TransportFamily::Hds {
        return Err(YtDlpTransportRequestError::HdsTransport);
    }
    if probe_container != ContainerFamily::F4f {
        return Err(YtDlpTransportRequestError::HdsContainer);
    }
    Ok(component)
}

/// Маппит service role в transport vocabulary без ordinal semantics.
const fn media_component_role(role: YtDlpCandidateComponentRole) -> MediaComponentRole {
    match role {
        YtDlpCandidateComponentRole::Muxed => MediaComponentRole::Muxed,
        YtDlpCandidateComponentRole::ContentProbed => MediaComponentRole::ContentProbed,
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
        (StreamLayout::HlsMuxedCodecDeferred(component), YtDlpCandidateComponentRole::Muxed) => {
            component.container().consistent_family().ok().flatten()
        }
        (StreamLayout::ContentProbed(component), YtDlpCandidateComponentRole::ContentProbed) => {
            Some(component.probe_container())
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
