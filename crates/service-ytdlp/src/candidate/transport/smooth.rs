//! Projection exact Smooth Streaming candidate-а в один presentation-level request.

use source_core::HttpRequestTarget;
use web_media_core::{
    AudioComponentDescriptor, CodecFamily, CodecKind, ContainerFamily, ContainerIdentity,
    MuxedComponentDescriptor, StreamLayout, TransportFamily, VideoComponentDescriptor,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, TransportOpenRequest,
};

use super::http_secret::smooth_manifest_secret_context;
use super::{PUBLIC_MEDIA_REDIRECT_HOPS, YtDlpTransportRequestContext, YtDlpTransportRequestError};
use crate::candidate::model::{
    YtDlpCandidateComponentRequest, YtDlpCandidateComponentRole, YtDlpNormalizedCandidate,
};

/// Два extractor resources могут совместно доказывать один presentation-level Manifest request.
struct SmoothManifestComponents<'candidate> {
    /// Video либо legacy muxed component задаёт единственный transport request.
    authority: &'candidate YtDlpCandidateComponentRequest,
    /// Separate audio component обязан подтвердить тот же target и request context.
    corroborating: Option<&'candidate YtDlpCandidateComponentRequest>,
}

impl YtDlpNormalizedCandidate {
    /// Строит один secret-scoped VOD request для exact H.264+AAC ISM presentation manifest-а.
    pub fn smooth_manifest_transport_request(
        &self,
        context: &YtDlpTransportRequestContext,
    ) -> Result<TransportOpenRequest, YtDlpTransportRequestError> {
        let components = smooth_manifest_components(self)?;
        let material = components
            .authority
            .material
            .smooth_manifest_request_material()
            .map_err(YtDlpTransportRequestError::SmoothRequestMaterial)?;
        let target = HttpRequestTarget::parse_exact(material.manifest_target_for_fetch())
            .map_err(YtDlpTransportRequestError::SmoothTarget)?;
        let secrets = smooth_manifest_secret_context(&material, &target)?;
        corroborate_presentation_request(components.corroborating, &material, &secrets)?;

        let identity = MediaComponentIdentity::new(
            self.descriptor().identity().clone(),
            self.descriptor().semantic_identity().clone(),
            MediaComponentRole::PresentationManifest,
        )
        .map_err(YtDlpTransportRequestError::Identity)?;
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
}

/// Separate audio resource обязан подтвердить target и effective authorization video resource-а.
fn corroborate_presentation_request(
    corroborating: Option<&YtDlpCandidateComponentRequest>,
    authority_material: &crate::candidate::request_material::YtDlpSmoothManifestRequestMaterial<'_>,
    authority_secrets: &web_media_transport_api::SecretRequestContext,
) -> Result<(), YtDlpTransportRequestError> {
    let Some(corroborating) = corroborating else {
        return Ok(());
    };
    let corroborating_material = corroborating
        .material
        .smooth_manifest_request_material()
        .map_err(YtDlpTransportRequestError::SmoothRequestMaterial)?;
    if corroborating_material.manifest_target_for_fetch()
        != authority_material.manifest_target_for_fetch()
    {
        return Err(YtDlpTransportRequestError::SmoothPresentationTargetMismatch);
    }
    let corroborating_target =
        HttpRequestTarget::parse_exact(corroborating_material.manifest_target_for_fetch())
            .map_err(YtDlpTransportRequestError::SmoothTarget)?;
    let corroborating_secrets =
        smooth_manifest_secret_context(&corroborating_material, &corroborating_target)?;
    if corroborating_secrets != *authority_secrets {
        return Err(YtDlpTransportRequestError::SmoothPresentationRequestContextMismatch);
    }
    Ok(())
}

/// Доказывает exact candidate-level Smooth Streaming profile до material projection.
fn smooth_manifest_components(
    candidate: &YtDlpNormalizedCandidate,
) -> Result<SmoothManifestComponents<'_>, YtDlpTransportRequestError> {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(muxed) => smooth_muxed_manifest_components(candidate, muxed),
        StreamLayout::Separate { video, audio } => {
            smooth_separate_manifest_components(candidate, video, audio)
        }
        StreamLayout::VideoOnly(_)
        | StreamLayout::AudioOnly(_)
        | StreamLayout::HlsMuxedCodecDeferred(_)
        | StreamLayout::ContentProbed(_) => Err(YtDlpTransportRequestError::SmoothLayout),
    }
}

/// Доказывает legacy muxed descriptor, который всё равно проецируется в Manifest resource.
fn smooth_muxed_manifest_components<'candidate>(
    candidate: &'candidate YtDlpNormalizedCandidate,
    muxed: &MuxedComponentDescriptor,
) -> Result<SmoothManifestComponents<'candidate>, YtDlpTransportRequestError> {
    let [component] = candidate.component_requests.as_ref() else {
        return Err(YtDlpTransportRequestError::SmoothComponentCount);
    };
    if component.role != YtDlpCandidateComponentRole::Muxed {
        return Err(YtDlpTransportRequestError::SmoothComponentRole);
    }
    validate_smooth_transport_and_container(muxed.transport().family(), muxed.container())?;
    validate_smooth_codecs(muxed.video().codec().kind(), muxed.audio().codec().kind())?;
    Ok(SmoothManifestComponents {
        authority: component,
        corroborating: None,
    })
}

/// Доказывает exact separate A/V descriptor и выбирает video request как authority.
fn smooth_separate_manifest_components<'candidate>(
    candidate: &'candidate YtDlpNormalizedCandidate,
    video: &VideoComponentDescriptor,
    audio: &AudioComponentDescriptor,
) -> Result<SmoothManifestComponents<'candidate>, YtDlpTransportRequestError> {
    let [first, second] = candidate.component_requests.as_ref() else {
        return Err(YtDlpTransportRequestError::SmoothComponentCount);
    };
    let (video_request, audio_request) = match (first.role, second.role) {
        (YtDlpCandidateComponentRole::Video, YtDlpCandidateComponentRole::Audio) => (first, second),
        (YtDlpCandidateComponentRole::Audio, YtDlpCandidateComponentRole::Video) => (second, first),
        _ => return Err(YtDlpTransportRequestError::SmoothComponentRole),
    };
    validate_smooth_transport_and_container(video.transport().family(), video.container())?;
    validate_smooth_transport_and_container(audio.transport().family(), audio.container())?;
    validate_smooth_codecs(video.video().codec().kind(), audio.audio().codec().kind())?;
    Ok(SmoothManifestComponents {
        authority: video_request,
        corroborating: Some(audio_request),
    })
}

/// Проверяет transport/container одного Smooth descriptor component-а без request material side effects.
fn validate_smooth_transport_and_container(
    transport: TransportFamily,
    container: &ContainerIdentity,
) -> Result<(), YtDlpTransportRequestError> {
    if transport != TransportFamily::SmoothStreaming {
        return Err(YtDlpTransportRequestError::SmoothTransport);
    }
    if container.consistent_family().ok() != Some(Some(ContainerFamily::FragmentedIsoBmff)) {
        return Err(YtDlpTransportRequestError::SmoothContainer);
    }
    Ok(())
}

/// Проверяет approved H.264+AAC codec profile без ослабления unknown/missing evidence.
fn validate_smooth_codecs(
    video_codec: CodecKind,
    audio_codec: CodecKind,
) -> Result<(), YtDlpTransportRequestError> {
    if video_codec != CodecKind::Known(CodecFamily::H264) {
        return Err(YtDlpTransportRequestError::SmoothVideoCodec);
    }
    if audio_codec != CodecKind::Known(CodecFamily::Aac) {
        return Err(YtDlpTransportRequestError::SmoothAudioCodec);
    }
    Ok(())
}
