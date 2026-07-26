//! Pure manifest-to-F2/F1/C3 projection для default и discovered qualities.

mod publication;

pub(crate) use publication::publish_catalog;

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use sha2::{Digest, Sha256};
use smooth_streaming_fmp4::{
    SmoothInitializationRequest, SmoothStreamOrdinal, SmoothTrackMappingRequest,
    SmoothTrackSelection, build_smooth_initialization_segment, map_smooth_track,
};
use smooth_streaming_manifest_core::{
    SmoothAudioQuality, SmoothCustomAttributeSet, SmoothManifest, SmoothQualityLevel,
    SmoothVideoQuality,
};
use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, Bitrate, ChannelCount, ComponentKind,
    ComponentVariantCatalog, ComponentVariantCatalogEntries, ComponentVariantCatalogIdentity,
    ComponentVariantCompatibilityEntries, ComponentVariantExactIdentity, ComponentVariantExactKey,
    ComponentVariantSelection, ComponentVariantSelectionRequest, ComponentVariantSemanticIdentity,
    ComponentVariantSemanticKey, DynamicRange, LanguageTag, NormalizedCodec, PreferredHeightPolicy,
    RawCodecIdentity, SampleRate, SemanticIdentity, VideoComponentVariant, VideoHeight,
    VideoTrackDescriptor, VideoWidth,
};

use crate::error::{
    SmoothPrepareError, SmoothProfileError, SmoothSemanticKeyError, SmoothSiblingRejection,
    SmoothSiblingRejectionReason,
};
use crate::model::SmoothRuntimeRow;
use crate::policy::SmoothPreparationPolicy;

/// Результат атомарной materialization всех catalog rows.
pub(crate) struct SmoothCatalogBuild {
    pub(crate) catalog: ComponentVariantCatalog,
    pub(crate) provider_selection: ComponentVariantSelection,
    pub(crate) video_rows: Box<[SmoothRuntimeRow]>,
    pub(crate) audio_rows: Box<[SmoothRuntimeRow]>,
}

/// Именованный internal request не смешивает два stream ordinal и policies.
pub(crate) struct SmoothCatalogBuildRequest<'request> {
    pub(crate) manifest: &'request Arc<SmoothManifest>,
    pub(crate) catalog_identity: ComponentVariantCatalogIdentity,
    pub(crate) parent_semantic: &'request SemanticIdentity,
    pub(crate) video_stream_ordinal: usize,
    pub(crate) audio_stream_ordinal: usize,
    pub(crate) preferred_height: PreferredHeightPolicy,
    pub(crate) policy: &'request SmoothPreparationPolicy,
    pub(crate) cancellation: &'request dyn Fn() -> bool,
}

/// Sibling-isolating materialization до transport/content proof и publication.
pub(crate) struct SmoothCatalogCandidates {
    pub(crate) video_rows: Vec<PendingVideoRow>,
    pub(crate) audio_rows: Vec<PendingAudioRow>,
    pub(crate) rejections: Vec<SmoothSiblingRejection>,
}

/// Временная video row хранит sort fields рядом с opaque C3 identity.
pub(crate) struct PendingVideoRow {
    height: u32,
    width: u32,
    bitrate: u64,
    canonical_key: String,
    pub(crate) variant: VideoComponentVariant,
    pub(crate) runtime: SmoothRuntimeRow,
}

/// Временная audio row хранит sort fields рядом с opaque C3 identity.
pub(crate) struct PendingAudioRow {
    bitrate: u64,
    sample_rate: u32,
    channels: u16,
    canonical_key: String,
    pub(crate) variant: AudioComponentVariant,
    pub(crate) runtime: SmoothRuntimeRow,
}

/// Строит только provider-default pair; sibling init/materialization сюда не входит.
pub(crate) fn build_provider_default_catalog(
    request: SmoothCatalogBuildRequest<'_>,
) -> Result<SmoothCatalogBuild, SmoothPrepareError> {
    let video_stream = &request.manifest.streams()[request.video_stream_ordinal];
    let mut video_candidates = Vec::with_capacity(video_stream.qualities().len());
    for quality in video_stream.qualities() {
        let SmoothQualityLevel::Video(video) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        video_candidates.push((
            video,
            canonical_video_key(video).map_err(SmoothPrepareError::SemanticKey)?,
        ));
    }
    video_candidates.sort_by(|(left, left_key), (right, right_key)| {
        request
            .preferred_height
            .compare(
                VideoHeight::new(left.height().get()).ok(),
                VideoHeight::new(right.height().get()).ok(),
            )
            .then_with(|| right.height().get().cmp(&left.height().get()))
            .then_with(|| right.width().get().cmp(&left.width().get()))
            .then_with(|| right.bitrate().get().cmp(&left.bitrate().get()))
            .then_with(|| left_key.cmp(right_key))
    });
    let selected_video = video_candidates
        .first()
        .ok_or(SmoothProfileError::EmptyQualityAxis)?;

    let audio_stream = &request.manifest.streams()[request.audio_stream_ordinal];
    let language = audio_stream.language().map(|value| value.as_str());
    let mut audio_candidates = Vec::with_capacity(audio_stream.qualities().len());
    for quality in audio_stream.qualities() {
        let SmoothQualityLevel::Audio(audio) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        audio_candidates.push((
            audio,
            canonical_audio_key(audio, language).map_err(SmoothPrepareError::SemanticKey)?,
        ));
    }
    audio_candidates.sort_by(|(left, left_key), (right, right_key)| {
        right
            .bitrate()
            .get()
            .cmp(&left.bitrate().get())
            .then_with(|| right.sampling_rate().get().cmp(&left.sampling_rate().get()))
            .then_with(|| right.channels().get().cmp(&left.channels().get()))
            .then_with(|| left_key.cmp(right_key))
    });
    let selected_audio = audio_candidates
        .first()
        .ok_or(SmoothProfileError::EmptyQualityAxis)?;

    let mut aggregate_initialization_bytes = 0;
    let video_row = build_video_row(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.video_stream_ordinal,
        selected_video.0,
        selected_video.1.clone(),
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;
    let audio_row = build_audio_row(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.audio_stream_ordinal,
        selected_audio.0,
        selected_audio.1.clone(),
        language,
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;
    publish_catalog(request, vec![video_row], vec![audio_row])
}

/// Materializes siblings independently; only job fences remain fatal here.
pub(crate) fn build_catalog_candidates(
    request: &SmoothCatalogBuildRequest<'_>,
) -> Result<SmoothCatalogCandidates, SmoothPrepareError> {
    let mut aggregate_initialization_bytes = 0_usize;
    let mut video_rows = Vec::new();
    let mut audio_rows = Vec::new();
    let mut rejections = Vec::new();

    for quality in request.manifest.streams()[request.video_stream_ordinal].qualities() {
        let SmoothQualityLevel::Video(video) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        let row = canonical_video_key(video)
            .map_err(SmoothPrepareError::SemanticKey)
            .and_then(|canonical_key| {
                build_video_row(
                    request.manifest,
                    request.catalog_identity.clone(),
                    request.parent_semantic,
                    request.video_stream_ordinal,
                    video,
                    canonical_key,
                    request.policy,
                    request.cancellation,
                    &mut aggregate_initialization_bytes,
                )
            });
        collect_candidate(row, ComponentKind::Video, &mut video_rows, &mut rejections)?;
    }
    for quality in request.manifest.streams()[request.audio_stream_ordinal].qualities() {
        let SmoothQualityLevel::Audio(audio) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        let language = request.manifest.streams()[request.audio_stream_ordinal]
            .language()
            .map(|value| value.as_str());
        let row = canonical_audio_key(audio, language)
            .map_err(SmoothPrepareError::SemanticKey)
            .and_then(|canonical_key| {
                build_audio_row(
                    request.manifest,
                    request.catalog_identity.clone(),
                    request.parent_semantic,
                    request.audio_stream_ordinal,
                    audio,
                    canonical_key,
                    language,
                    request.policy,
                    request.cancellation,
                    &mut aggregate_initialization_bytes,
                )
            });
        collect_candidate(row, ComponentKind::Audio, &mut audio_rows, &mut rejections)?;
    }

    reject_ambiguous_video_rows(&mut video_rows, &mut rejections);
    reject_ambiguous_audio_rows(&mut audio_rows, &mut rejections);
    require_not_cancelled(request.cancellation)?;
    Ok(SmoothCatalogCandidates {
        video_rows,
        audio_rows,
        rejections,
    })
}

fn collect_candidate<Row>(
    row: Result<Row, SmoothPrepareError>,
    component: ComponentKind,
    rows: &mut Vec<Row>,
    rejections: &mut Vec<SmoothSiblingRejection>,
) -> Result<(), SmoothPrepareError> {
    match row {
        Ok(row) => rows.push(row),
        Err(error) => {
            let Some(reason) = isolatable_row_error(&error) else {
                return Err(error);
            };
            rejections.push(SmoothSiblingRejection::new(component, reason));
        }
    }
    Ok(())
}

fn isolatable_row_error(error: &SmoothPrepareError) -> Option<SmoothSiblingRejectionReason> {
    match error {
        SmoothPrepareError::Mapping(_) => Some(SmoothSiblingRejectionReason::TrackMapping),
        SmoothPrepareError::Initialization(_) => Some(SmoothSiblingRejectionReason::Initialization),
        SmoothPrepareError::VariantKey(_)
        | SmoothPrepareError::SemanticKey(_)
        | SmoothPrepareError::Profile(SmoothProfileError::DescriptorBounds) => {
            Some(SmoothSiblingRejectionReason::UnsupportedMetadata)
        }
        _ => None,
    }
}

fn reject_ambiguous_video_rows(
    rows: &mut Vec<PendingVideoRow>,
    rejections: &mut Vec<SmoothSiblingRejection>,
) {
    let counts = semantic_counts(rows.iter().map(|row| row.canonical_key.as_str()));
    rows.retain(|row| {
        retain_unique_semantic_row(
            &counts,
            &row.canonical_key,
            ComponentKind::Video,
            rejections,
        )
    });
}

fn reject_ambiguous_audio_rows(
    rows: &mut Vec<PendingAudioRow>,
    rejections: &mut Vec<SmoothSiblingRejection>,
) {
    let counts = semantic_counts(rows.iter().map(|row| row.canonical_key.as_str()));
    rows.retain(|row| {
        retain_unique_semantic_row(
            &counts,
            &row.canonical_key,
            ComponentKind::Audio,
            rejections,
        )
    });
}

fn semantic_counts<'a>(keys: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for key in keys {
        *counts.entry(key.to_owned()).or_insert(0) += 1;
    }
    counts
}

fn retain_unique_semantic_row(
    counts: &HashMap<String, usize>,
    key: &str,
    component: ComponentKind,
    rejections: &mut Vec<SmoothSiblingRejection>,
) -> bool {
    if counts.get(key).copied() == Some(1) {
        true
    } else {
        rejections.push(SmoothSiblingRejection::new(
            component,
            SmoothSiblingRejectionReason::AmbiguousSemanticIdentity,
        ));
        false
    }
}

/// Отображает каждое качество обеих осей и публикует catalog только целиком.
#[cfg(test)]
pub(crate) fn build_catalog(
    request: SmoothCatalogBuildRequest<'_>,
) -> Result<SmoothCatalogBuild, SmoothPrepareError> {
    let mut aggregate_initialization_bytes = 0_usize;
    let video_rows = build_video_rows(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.video_stream_ordinal,
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;
    let audio_rows = build_audio_rows(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.audio_stream_ordinal,
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;

    publish_catalog(request, video_rows, audio_rows)
}

/// Единый publication fence для cancellation между завершёнными стадиями.
pub(super) fn require_not_cancelled(
    cancellation: &dyn Fn() -> bool,
) -> Result<(), SmoothPrepareError> {
    if cancellation() {
        Err(SmoothPrepareError::Cancelled)
    } else {
        Ok(())
    }
}

/// Строит video rows, включая exact F2 mapping и owned F1 init.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn build_video_rows(
    manifest: &SmoothManifest,
    catalog_identity: ComponentVariantCatalogIdentity,
    parent_semantic: &SemanticIdentity,
    stream_ordinal: usize,
    policy: &SmoothPreparationPolicy,
    cancellation: &dyn Fn() -> bool,
    aggregate_initialization_bytes: &mut usize,
) -> Result<Vec<PendingVideoRow>, SmoothPrepareError> {
    let stream = &manifest.streams()[stream_ordinal];
    let mut rows = Vec::with_capacity(stream.qualities().len());
    for quality in stream.qualities() {
        let SmoothQualityLevel::Video(video) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        let canonical_key = canonical_video_key(video).map_err(SmoothPrepareError::SemanticKey)?;
        rows.push(build_video_row(
            manifest,
            catalog_identity.clone(),
            parent_semantic,
            stream_ordinal,
            video,
            canonical_key,
            policy,
            cancellation,
            aggregate_initialization_bytes,
        )?);
    }
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn build_video_row(
    manifest: &SmoothManifest,
    catalog_identity: ComponentVariantCatalogIdentity,
    parent_semantic: &SemanticIdentity,
    stream_ordinal: usize,
    video: &SmoothVideoQuality,
    canonical_key: String,
    policy: &SmoothPreparationPolicy,
    cancellation: &dyn Fn() -> bool,
    aggregate_initialization_bytes: &mut usize,
) -> Result<PendingVideoRow, SmoothPrepareError> {
    let identities = variant_identities(
        &catalog_identity,
        parent_semantic,
        ComponentKind::Video,
        &canonical_key,
    )?;
    let selection =
        SmoothTrackSelection::new(SmoothStreamOrdinal::new(stream_ordinal), video.index());
    let initialization = build_initialization(
        manifest,
        selection,
        policy,
        cancellation,
        aggregate_initialization_bytes,
    )?;
    let descriptor = video_descriptor(video)?;
    let variant = VideoComponentVariant::new(identities.0.clone(), identities.1, descriptor);
    Ok(PendingVideoRow {
        height: video.height().get(),
        width: video.width().get(),
        bitrate: video.bitrate().get(),
        canonical_key,
        runtime: runtime_row(identities.0, selection, initialization),
        variant,
    })
}

/// Строит audio rows, сохраняя stream-level language в descriptor и identity.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn build_audio_rows(
    manifest: &SmoothManifest,
    catalog_identity: ComponentVariantCatalogIdentity,
    parent_semantic: &SemanticIdentity,
    stream_ordinal: usize,
    policy: &SmoothPreparationPolicy,
    cancellation: &dyn Fn() -> bool,
    aggregate_initialization_bytes: &mut usize,
) -> Result<Vec<PendingAudioRow>, SmoothPrepareError> {
    let stream = &manifest.streams()[stream_ordinal];
    let language = stream.language().map(|value| value.as_str());
    let mut rows = Vec::with_capacity(stream.qualities().len());
    for quality in stream.qualities() {
        let SmoothQualityLevel::Audio(audio) = quality else {
            return Err(SmoothProfileError::QualityKindMismatch.into());
        };
        let canonical_key =
            canonical_audio_key(audio, language).map_err(SmoothPrepareError::SemanticKey)?;
        rows.push(build_audio_row(
            manifest,
            catalog_identity.clone(),
            parent_semantic,
            stream_ordinal,
            audio,
            canonical_key,
            language,
            policy,
            cancellation,
            aggregate_initialization_bytes,
        )?);
    }
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn build_audio_row(
    manifest: &SmoothManifest,
    catalog_identity: ComponentVariantCatalogIdentity,
    parent_semantic: &SemanticIdentity,
    stream_ordinal: usize,
    audio: &SmoothAudioQuality,
    canonical_key: String,
    language: Option<&str>,
    policy: &SmoothPreparationPolicy,
    cancellation: &dyn Fn() -> bool,
    aggregate_initialization_bytes: &mut usize,
) -> Result<PendingAudioRow, SmoothPrepareError> {
    let identities = variant_identities(
        &catalog_identity,
        parent_semantic,
        ComponentKind::Audio,
        &canonical_key,
    )?;
    let selection =
        SmoothTrackSelection::new(SmoothStreamOrdinal::new(stream_ordinal), audio.index());
    let initialization = build_initialization(
        manifest,
        selection,
        policy,
        cancellation,
        aggregate_initialization_bytes,
    )?;
    let descriptor = audio_descriptor(audio, language)?;
    let variant = AudioComponentVariant::new(identities.0.clone(), identities.1, descriptor);
    Ok(PendingAudioRow {
        bitrate: audio.bitrate().get(),
        sample_rate: audio.sampling_rate().get(),
        channels: audio.channels().get(),
        canonical_key,
        runtime: runtime_row(identities.0, selection, initialization),
        variant,
    })
}

/// Строит F1 init и применяет общий aggregate budget после каждого качества.
fn build_initialization(
    manifest: &SmoothManifest,
    selection: SmoothTrackSelection,
    policy: &SmoothPreparationPolicy,
    cancellation: &dyn Fn() -> bool,
    aggregate_initialization_bytes: &mut usize,
) -> Result<smooth_streaming_fmp4::SmoothInitializationSegment, SmoothPrepareError> {
    let mapped = map_smooth_track(SmoothTrackMappingRequest::new(
        manifest,
        selection,
        cancellation,
    ))
    .map_err(|error| {
        if matches!(
            error,
            smooth_streaming_fmp4::SmoothTrackMappingError::Cancelled
        ) || cancellation()
        {
            SmoothPrepareError::Cancelled
        } else {
            SmoothPrepareError::Mapping(error)
        }
    })?;
    let initialization = build_smooth_initialization_segment(SmoothInitializationRequest::new(
        &mapped,
        &policy.initialization_limits,
        cancellation,
    ))
    .map_err(|error| {
        if matches!(
            error,
            smooth_streaming_fmp4::SmoothInitializationError::Cancelled
        ) || cancellation()
        {
            SmoothPrepareError::Cancelled
        } else {
            SmoothPrepareError::Initialization(error)
        }
    })?;
    *aggregate_initialization_bytes = aggregate_initialization_bytes
        .checked_add(initialization.initialization_segment_bytes().len())
        .ok_or(SmoothProfileError::AggregateInitializationLimit)?;
    if *aggregate_initialization_bytes > policy.aggregate_initialization_limit.maximum_bytes() {
        return Err(SmoothProfileError::AggregateInitializationLimit.into());
    }
    Ok(initialization)
}

/// Сохраняет immutable init evidence в cloneable private runtime row.
fn runtime_row(
    exact_identity: ComponentVariantExactIdentity,
    selection: SmoothTrackSelection,
    initialization: smooth_streaming_fmp4::SmoothInitializationSegment,
) -> SmoothRuntimeRow {
    let initialization_identity = initialization.identity();
    SmoothRuntimeRow {
        exact_identity,
        selection,
        initialization_identity,
        initialization_bytes: Bytes::from(initialization.into_initialization_segment_bytes()),
    }
}

/// Создаёт C3 exact+semantic identities из одной versioned canonical key.
fn variant_identities(
    catalog: &ComponentVariantCatalogIdentity,
    parent_semantic: &SemanticIdentity,
    component: ComponentKind,
    canonical_key: &str,
) -> Result<
    (
        ComponentVariantExactIdentity,
        ComponentVariantSemanticIdentity,
    ),
    SmoothPrepareError,
> {
    let exact_key =
        ComponentVariantExactKey::new(canonical_key).map_err(SmoothPrepareError::VariantKey)?;
    let semantic_key =
        ComponentVariantSemanticKey::new(canonical_key).map_err(SmoothPrepareError::VariantKey)?;
    Ok((
        ComponentVariantExactIdentity::new(catalog.clone(), component, exact_key),
        ComponentVariantSemanticIdentity::new(parent_semantic.clone(), component, semantic_key),
    ))
}

/// Нормализует H.264 video metadata в C3 descriptor без выдуманного FPS.
fn video_descriptor(
    quality: &SmoothVideoQuality,
) -> Result<VideoTrackDescriptor, SmoothPrepareError> {
    let codec = NormalizedCodec::parse(
        RawCodecIdentity::new("h264").map_err(|_| SmoothProfileError::DescriptorBounds)?,
    );
    let width =
        VideoWidth::new(quality.width().get()).map_err(|_| SmoothProfileError::DescriptorBounds)?;
    let height = VideoHeight::new(quality.height().get())
        .map_err(|_| SmoothProfileError::DescriptorBounds)?;
    let bitrate =
        Bitrate::new(quality.bitrate().get()).map_err(|_| SmoothProfileError::DescriptorBounds)?;
    Ok(VideoTrackDescriptor::new(
        codec,
        Some(width),
        Some(height),
        None,
        Some(bitrate),
        DynamicRange::Sdr,
    ))
}

/// Нормализует AAC-LC audio metadata в C3 descriptor.
fn audio_descriptor(
    quality: &SmoothAudioQuality,
    language: Option<&str>,
) -> Result<AudioTrackDescriptor, SmoothPrepareError> {
    let codec = NormalizedCodec::parse(
        RawCodecIdentity::new("mp4a.40.2").map_err(|_| SmoothProfileError::DescriptorBounds)?,
    );
    let sample_rate = SampleRate::new(quality.sampling_rate().get())
        .map_err(|_| SmoothProfileError::DescriptorBounds)?;
    let channels = ChannelCount::new(quality.channels().get())
        .map_err(|_| SmoothProfileError::DescriptorBounds)?;
    let bitrate =
        Bitrate::new(quality.bitrate().get()).map_err(|_| SmoothProfileError::DescriptorBounds)?;
    let language = language
        .map(LanguageTag::new)
        .transpose()
        .map_err(|_| SmoothProfileError::DescriptorBounds)?;
    Ok(AudioTrackDescriptor::new(
        codec,
        Some(sample_rate),
        Some(channels),
        Some(bitrate),
        language,
    ))
}

/// Video semantic fields исключают ordinal/index/clock/template/URL/codec bytes.
fn canonical_video_key(quality: &SmoothVideoQuality) -> Result<String, SmoothSemanticKeyError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ss-v1-v")?;
    hash_field(&mut hasher, b"h264")?;
    hash_field(&mut hasher, &quality.width().get().to_be_bytes())?;
    hash_field(&mut hasher, &quality.height().get().to_be_bytes())?;
    hash_field(&mut hasher, &quality.bitrate().get().to_be_bytes())?;
    hash_custom_attributes(&mut hasher, quality.custom_attributes())?;
    render_semantic_key("ss-v1-v", hasher)
}

/// Audio semantic fields исключают ordinal/index/clock/template/URL/codec bytes.
fn canonical_audio_key(
    quality: &SmoothAudioQuality,
    language: Option<&str>,
) -> Result<String, SmoothSemanticKeyError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ss-v1-a")?;
    hash_field(&mut hasher, b"aac-lc")?;
    hash_field(&mut hasher, &quality.bitrate().get().to_be_bytes())?;
    hash_field(&mut hasher, &quality.sampling_rate().get().to_be_bytes())?;
    hash_field(&mut hasher, &quality.channels().get().to_be_bytes())?;
    hash_field(&mut hasher, language.unwrap_or_default().as_bytes())?;
    hash_custom_attributes(&mut hasher, quality.custom_attributes())?;
    render_semantic_key("ss-v1-a", hasher)
}

/// Length framing исключает неоднозначность concatenated variable-length fields.
fn hash_field(hasher: &mut Sha256, field: &[u8]) -> Result<(), SmoothSemanticKeyError> {
    let length =
        u64::try_from(field.len()).map_err(|_| SmoothSemanticKeyError::FieldLengthOutOfRange)?;
    hasher.update(length.to_be_bytes());
    hasher.update(field);
    Ok(())
}

/// Custom attributes канонизируются независимо от XML order.
fn hash_custom_attributes(
    hasher: &mut Sha256,
    attributes: &SmoothCustomAttributeSet,
) -> Result<(), SmoothSemanticKeyError> {
    let mut sorted = attributes.as_slice().iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        left.name()
            .as_str()
            .cmp(right.name().as_str())
            .then_with(|| left.value().as_str().cmp(right.value().as_str()))
    });
    let attribute_count =
        u64::try_from(sorted.len()).map_err(|_| SmoothSemanticKeyError::FieldLengthOutOfRange)?;
    hash_field(hasher, &attribute_count.to_be_bytes())?;
    for attribute in sorted {
        hash_field(hasher, attribute.name().as_str().as_bytes())?;
        hash_field(hasher, attribute.value().as_str().as_bytes())?;
    }
    Ok(())
}

/// Собирает handoff key с checked output arithmetic.
fn render_semantic_key(
    output_prefix: &str,
    hasher: Sha256,
) -> Result<String, SmoothSemanticKeyError> {
    let digest_hex = format!("{:x}", hasher.finalize());
    let output_capacity = output_prefix
        .len()
        .checked_add(1)
        .and_then(|prefix_with_separator| prefix_with_separator.checked_add(digest_hex.len()))
        .ok_or(SmoothSemanticKeyError::OutputLengthOverflow)?;
    let mut output = String::with_capacity(output_capacity);
    output.push_str(output_prefix);
    output.push('-');
    output.push_str(&digest_hex);
    Ok(output)
}

#[cfg(test)]
mod tests;
