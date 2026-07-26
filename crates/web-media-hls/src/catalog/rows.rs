use hls_playlist_core::{HlsVideoRange, MediaRendition, VariantStream};
use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, ChannelCount, ComponentKind,
    ComponentVariantCatalogIdentity, ComponentVariantExactIdentity, ComponentVariantExactKey,
    ComponentVariantSemanticIdentity, ComponentVariantSemanticKey, CoupledComponentVariant,
    CoupledVariantExactIdentity, CoupledVariantSemanticIdentity, DynamicRange, FrameRate,
    LanguageTag, VideoComponentVariant, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

use super::identity::SemanticKeyBuilder;
use super::{HlsCatalogBuildError, HlsCatalogSiblingRejectionReason};
use crate::HlsRequiredContainer;

pub(super) fn build_video_row(
    catalog: &ComponentVariantCatalogIdentity,
    variant: &VariantStream,
    container: HlsRequiredContainer,
    proof: &VideoTrackDescriptor,
) -> Result<(String, VideoComponentVariant), HlsCatalogSiblingRejectionReason> {
    let descriptor = merge_video_evidence(variant, proof)?;
    let key = variant_video_key("hls-v1-video", variant, container, &descriptor)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let (exact, semantic) = component_identities(catalog, ComponentKind::Video, &key)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    Ok((key, VideoComponentVariant::new(exact, semantic, descriptor)))
}

pub(super) fn build_variant_audio_row(
    catalog: &ComponentVariantCatalogIdentity,
    variant: &VariantStream,
    container: HlsRequiredContainer,
    proof: &AudioTrackDescriptor,
) -> Result<(String, AudioComponentVariant), HlsCatalogSiblingRejectionReason> {
    let mut key = SemanticKeyBuilder::new("hls-v1-audio-variant")
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_variant(&mut key, variant)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_container(&mut key, container)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    key.audio(proof)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let key = key
        .finish()
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let (exact, semantic) = component_identities(catalog, ComponentKind::Audio, &key)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    Ok((
        key,
        AudioComponentVariant::new(exact, semantic, proof.clone()),
    ))
}

pub(super) fn build_rendition_audio_row(
    catalog: &ComponentVariantCatalogIdentity,
    rendition: &MediaRendition,
    container: HlsRequiredContainer,
    proof: &AudioTrackDescriptor,
) -> Result<(String, AudioComponentVariant), HlsCatalogSiblingRejectionReason> {
    let descriptor = merge_audio_evidence(rendition, proof)?;
    let mut key = SemanticKeyBuilder::new("hls-v1-audio-rendition")
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_rendition(&mut key, rendition)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_container(&mut key, container)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    key.audio(&descriptor)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let key = key
        .finish()
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let (exact, semantic) = component_identities(catalog, ComponentKind::Audio, &key)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    Ok((key, AudioComponentVariant::new(exact, semantic, descriptor)))
}

pub(super) fn build_coupled_row(
    catalog: &ComponentVariantCatalogIdentity,
    variant: &VariantStream,
    container: HlsRequiredContainer,
    video: &VideoTrackDescriptor,
    audio: &AudioTrackDescriptor,
) -> Result<(String, CoupledComponentVariant), HlsCatalogSiblingRejectionReason> {
    let video = merge_video_evidence(variant, video)?;
    let mut key = SemanticKeyBuilder::new("hls-v1-coupled")
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_variant(&mut key, variant)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    hash_container(&mut key, container)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    key.video(&video)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    key.audio(audio)
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let key = key
        .finish()
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let exact_key = ComponentVariantExactKey::new(key.clone())
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    let semantic_key = ComponentVariantSemanticKey::new(key.clone())
        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
    Ok((
        key,
        CoupledComponentVariant::new(
            CoupledVariantExactIdentity::new(catalog.clone(), exact_key),
            CoupledVariantSemanticIdentity::new(catalog.parent().semantic().clone(), semantic_key),
            video,
            audio.clone(),
        ),
    ))
}

fn component_identities(
    catalog: &ComponentVariantCatalogIdentity,
    kind: ComponentKind,
    key: &str,
) -> Result<
    (
        ComponentVariantExactIdentity,
        ComponentVariantSemanticIdentity,
    ),
    HlsCatalogBuildError,
> {
    let exact_key = ComponentVariantExactKey::new(key.to_owned())
        .map_err(|_| HlsCatalogBuildError::SemanticIdentity)?;
    let semantic_key = ComponentVariantSemanticKey::new(key.to_owned())
        .map_err(|_| HlsCatalogBuildError::SemanticIdentity)?;
    Ok((
        ComponentVariantExactIdentity::new(catalog.clone(), kind, exact_key),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            kind,
            semantic_key,
        ),
    ))
}

fn merge_video_evidence(
    variant: &VariantStream,
    proof: &VideoTrackDescriptor,
) -> Result<VideoTrackDescriptor, HlsCatalogSiblingRejectionReason> {
    let (width, height) = match variant.resolution {
        Some((width, height)) => {
            if proof.width_pixels().is_some_and(|actual| actual != width)
                || proof
                    .height()
                    .is_some_and(|actual| actual.pixels() != height)
            {
                return Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict);
            }
            (
                Some(
                    VideoWidth::new(width)
                        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?,
                ),
                Some(
                    VideoHeight::new(height)
                        .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?,
                ),
            )
        }
        None => (
            proof
                .width_pixels()
                .map(VideoWidth::new)
                .transpose()
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?,
            proof.height(),
        ),
    };
    let frame_rate = match variant.frame_rate {
        Some(rate) => {
            let numerator = u32::try_from(rate.numerator())
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
            let denominator = u32::try_from(rate.denominator())
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
            let declared = FrameRate::new(numerator, denominator)
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
            if proof.frame_rate().is_some_and(|actual| actual != declared) {
                return Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict);
            }
            Some(declared)
        }
        None => proof.frame_rate(),
    };
    let dynamic_range = match variant.video_range {
        Some(HlsVideoRange::Sdr) => merge_dynamic_range(DynamicRange::Sdr, proof.dynamic_range())?,
        Some(HlsVideoRange::Hlg | HlsVideoRange::Pq) => {
            merge_dynamic_range(DynamicRange::Hdr, proof.dynamic_range())?
        }
        None => proof.dynamic_range(),
    };
    Ok(VideoTrackDescriptor::new(
        proof.codec().clone(),
        width,
        height,
        frame_rate,
        // Aggregate HLS bandwidth намеренно не становится component bitrate.
        proof.bitrate(),
        dynamic_range,
    ))
}

fn merge_dynamic_range(
    declared: DynamicRange,
    actual: DynamicRange,
) -> Result<DynamicRange, HlsCatalogSiblingRejectionReason> {
    if actual != DynamicRange::Unknown && actual != declared {
        return Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict);
    }
    Ok(declared)
}

fn merge_audio_evidence(
    rendition: &MediaRendition,
    proof: &AudioTrackDescriptor,
) -> Result<AudioTrackDescriptor, HlsCatalogSiblingRejectionReason> {
    let channels = match rendition.channel_count {
        Some(channels) => {
            let count = u16::try_from(channels.get())
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
            let declared = ChannelCount::new(count)
                .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?;
            if proof.channels().is_some_and(|actual| actual != declared) {
                return Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict);
            }
            Some(declared)
        }
        None => proof.channels(),
    };
    let language = match rendition.language.as_deref() {
        Some(language) => {
            if proof
                .language()
                .is_some_and(|actual| actual.as_str() != language)
            {
                return Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict);
            }
            Some(
                LanguageTag::new(language)
                    .map_err(|_| HlsCatalogSiblingRejectionReason::DescriptorBounds)?,
            )
        }
        None => proof.language().cloned(),
    };
    Ok(AudioTrackDescriptor::new(
        proof.codec().clone(),
        proof.sample_rate(),
        channels,
        proof.bitrate(),
        language,
    ))
}

fn variant_video_key(
    prefix: &'static str,
    variant: &VariantStream,
    container: HlsRequiredContainer,
    video: &VideoTrackDescriptor,
) -> Result<String, HlsCatalogBuildError> {
    let mut key = SemanticKeyBuilder::new(prefix)?;
    hash_variant(&mut key, variant)?;
    hash_container(&mut key, container)?;
    key.video(video)?;
    key.finish()
}

fn hash_variant(
    key: &mut SemanticKeyBuilder,
    variant: &VariantStream,
) -> Result<(), HlsCatalogBuildError> {
    key.field(&variant.bandwidth.to_be_bytes())?;
    let average_bandwidth = variant.average_bandwidth.map(u64::to_be_bytes);
    key.optional_field(average_bandwidth.as_ref().map(|value| value.as_slice()))?;
    key.optional_field(variant.audio_group.as_deref().map(str::as_bytes))
}

fn hash_container(
    key: &mut SemanticKeyBuilder,
    container: HlsRequiredContainer,
) -> Result<(), HlsCatalogBuildError> {
    key.field(match container {
        HlsRequiredContainer::TransportStream => b"mpeg-ts",
        HlsRequiredContainer::FragmentedMp4 => b"fmp4",
    })
}

fn hash_rendition(
    key: &mut SemanticKeyBuilder,
    rendition: &MediaRendition,
) -> Result<(), HlsCatalogBuildError> {
    key.field(rendition.group_id.as_bytes())?;
    key.field(rendition.name.as_bytes())?;
    key.optional_field(rendition.language.as_deref().map(str::as_bytes))?;
    key.optional_field(rendition.associated_language.as_deref().map(str::as_bytes))?;
    key.optional_field(rendition.characteristics.as_deref().map(str::as_bytes))?;
    key.optional_field(rendition.channels.as_deref().map(str::as_bytes))?;
    key.field(&[u8::from(rendition.is_default)])?;
    key.field(&[u8::from(rendition.autoselect)])
}
