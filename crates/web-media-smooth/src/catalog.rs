//! Pure manifest-to-F2/F1/C3 projection для всех объявленных qualities.

use std::sync::Arc;

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
    ComponentVariantExactIdentity, ComponentVariantExactKey, ComponentVariantSelection,
    ComponentVariantSelectionRequest, ComponentVariantSemanticIdentity,
    ComponentVariantSemanticKey, DynamicRange, LanguageTag, NormalizedCodec, PreferredHeightPolicy,
    RawCodecIdentity, SampleRate, SemanticIdentity, VideoComponentVariant, VideoHeight,
    VideoTrackDescriptor, VideoWidth,
};

use crate::error::{SmoothPrepareError, SmoothProfileError, SmoothSemanticKeyError};
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

/// Временная video row хранит sort fields рядом с opaque C3 identity.
struct PendingVideoRow {
    height: u32,
    width: u32,
    bitrate: u64,
    canonical_key: String,
    variant: VideoComponentVariant,
    runtime: SmoothRuntimeRow,
}

/// Временная audio row хранит sort fields рядом с opaque C3 identity.
struct PendingAudioRow {
    bitrate: u64,
    sample_rate: u32,
    channels: u16,
    canonical_key: String,
    variant: AudioComponentVariant,
    runtime: SmoothRuntimeRow,
}

/// Отображает каждое качество обеих осей и публикует catalog только целиком.
pub(crate) fn build_catalog(
    request: SmoothCatalogBuildRequest<'_>,
) -> Result<SmoothCatalogBuild, SmoothPrepareError> {
    let mut aggregate_initialization_bytes = 0_usize;
    let mut video_rows = build_video_rows(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.video_stream_ordinal,
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;
    let mut audio_rows = build_audio_rows(
        request.manifest,
        request.catalog_identity.clone(),
        request.parent_semantic,
        request.audio_stream_ordinal,
        request.policy,
        request.cancellation,
        &mut aggregate_initialization_bytes,
    )?;

    video_rows.sort_by(|left, right| {
        right
            .height
            .cmp(&left.height)
            .then_with(|| right.width.cmp(&left.width))
            .then_with(|| right.bitrate.cmp(&left.bitrate))
            .then_with(|| left.canonical_key.cmp(&right.canonical_key))
    });
    audio_rows.sort_by(|left, right| {
        right
            .bitrate
            .cmp(&left.bitrate)
            .then_with(|| right.sample_rate.cmp(&left.sample_rate))
            .then_with(|| right.channels.cmp(&left.channels))
            .then_with(|| left.canonical_key.cmp(&right.canonical_key))
    });
    require_not_cancelled(request.cancellation)?;

    let video_variants = video_rows
        .iter()
        .map(|row| row.variant.clone())
        .collect::<Vec<_>>();
    let audio_variants = audio_rows
        .iter()
        .map(|row| row.variant.clone())
        .collect::<Vec<_>>();
    require_not_cancelled(request.cancellation)?;
    let catalog = ComponentVariantCatalog::new(
        request.catalog_identity,
        request.policy.catalog_limit,
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: video_variants,
            audio: audio_variants,
        },
    )
    .map_err(SmoothPrepareError::Catalog)?;

    let preferred_video = catalog
        .preferred_video_variant(request.preferred_height)
        .map_err(SmoothPrepareError::Catalog)?
        .exact_identity()
        .clone();
    let preferred_audio = catalog
        .required_audio_variants()
        .map_err(SmoothPrepareError::Catalog)?
        .first()
        .ok_or(SmoothProfileError::EmptyQualityAxis)?
        .exact_identity()
        .clone();
    let provider_selection = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: preferred_video,
            audio: preferred_audio,
        })
        .map_err(SmoothPrepareError::Catalog)?;

    let prepared = SmoothCatalogBuild {
        catalog,
        provider_selection,
        video_rows: video_rows
            .into_iter()
            .map(|row| row.runtime)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        audio_rows: audio_rows
            .into_iter()
            .map(|row| row.runtime)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    require_not_cancelled(request.cancellation)?;
    Ok(prepared)
}

/// Единый publication fence для cancellation между завершёнными стадиями.
fn require_not_cancelled(cancellation: &dyn Fn() -> bool) -> Result<(), SmoothPrepareError> {
    if cancellation() {
        Err(SmoothPrepareError::Cancelled)
    } else {
        Ok(())
    }
}

/// Строит video rows, включая exact F2 mapping и owned F1 init.
#[allow(clippy::too_many_arguments)]
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
        let canonical_key: String =
            canonical_video_key(video).map_err(SmoothPrepareError::SemanticKey)?;
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
        rows.push(PendingVideoRow {
            height: video.height().get(),
            width: video.width().get(),
            bitrate: video.bitrate().get(),
            canonical_key,
            runtime: SmoothRuntimeRow {
                exact_identity: identities.0,
                selection,
                initialization,
            },
            variant,
        });
    }
    Ok(rows)
}

/// Строит audio rows, сохраняя stream-level language в descriptor и identity.
#[allow(clippy::too_many_arguments)]
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
        let canonical_key: String =
            canonical_audio_key(audio, language).map_err(SmoothPrepareError::SemanticKey)?;
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
        rows.push(PendingAudioRow {
            bitrate: audio.bitrate().get(),
            sample_rate: audio.sampling_rate().get(),
            channels: audio.channels().get(),
            canonical_key,
            runtime: SmoothRuntimeRow {
                exact_identity: identities.0,
                selection,
                initialization,
            },
            variant,
        });
    }
    Ok(rows)
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
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use web_media_core::{ComponentVariantCatalog, PreferredHeightPolicy};

    use super::{
        SmoothCatalogBuildRequest, build_catalog, canonical_audio_key, canonical_video_key,
    };
    use crate::SmoothProfileError;
    use crate::test_support::{VALID_MANIFEST, catalog_identity, parse, policy};

    #[test]
    fn builds_init_for_every_quality_and_sorts_axes_deterministically() {
        let document = VALID_MANIFEST
            .replace("QualityLevels=\"1\"", "QualityLevels=\"2\"")
            .replacen(
                r#"</QualityLevel>"#,
                r#"</QualityLevel>
    <QualityLevel Index="1" Bitrate="750000" FourCC="H264" MaxWidth="1280" MaxHeight="720" CodecPrivateData="000000016742001E0000000168CE06E2"/>"#,
                1,
            )
            .replacen(
                r#"<QualityLevel Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>"#,
                r#"<QualityLevel Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>
    <QualityLevel Index="1" Bitrate="64000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>"#,
                1,
            );
        let manifest = Arc::new(parse(&document));
        let (identity, semantic) = catalog_identity();
        let policy = policy(64 * 1024);
        let built = build_catalog(SmoothCatalogBuildRequest {
            manifest: &manifest,
            catalog_identity: identity,
            parent_semantic: &semantic,
            video_stream_ordinal: 0,
            audio_stream_ordinal: 1,
            preferred_height: PreferredHeightPolicy::NoPreference,
            policy: &policy,
            cancellation: &|| false,
        })
        .expect("все качества обязаны материализоваться");

        assert_eq!(built.video_rows.len(), 2);
        assert_eq!(built.audio_rows.len(), 2);
        assert!(matches!(
            built.catalog,
            ComponentVariantCatalog::VideoAndAudio { .. }
        ));
        assert_eq!(
            built.video_rows[0].selection.quality_index.get(),
            0,
            "1080p row должна быть первой"
        );
        assert_eq!(
            built.audio_rows[0].selection.quality_index.get(),
            0,
            "128 kbps row должна быть первой"
        );
        assert!(
            built
                .video_rows
                .iter()
                .chain(built.audio_rows.iter())
                .all(|row| !row.initialization.initialization_segment_bytes().is_empty())
        );
    }

    #[test]
    fn aggregate_initialization_budget_fails_whole_catalog() {
        let manifest = Arc::new(parse(VALID_MANIFEST));
        let (identity, semantic) = catalog_identity();

        assert!(matches!(
            build_catalog(SmoothCatalogBuildRequest {
                manifest: &manifest,
                catalog_identity: identity,
                parent_semantic: &semantic,
                video_stream_ordinal: 0,
                audio_stream_ordinal: 1,
                preferred_height: PreferredHeightPolicy::NoPreference,
                policy: &policy(1),
                cancellation: &|| false,
            }),
            Err(crate::SmoothPrepareError::Profile(
                SmoothProfileError::AggregateInitializationLimit
            ))
        ));
    }

    #[test]
    fn canonical_keys_ignore_declared_index_and_xml_attribute_order() {
        let reordered = VALID_MANIFEST
            .replace(
                r#"Index="0" Bitrate="1500000" FourCC="H264" MaxWidth="1920" MaxHeight="1080""#,
                r#"MaxHeight="1080" FourCC="H264" Index="91" MaxWidth="1920" Bitrate="1500000""#,
            )
            .replace(
                r#"Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2""#,
                r#"Channels="2" SamplingRate="48000" Index="92" FourCC="AACL" Bitrate="128000""#,
            );
        let original = parse(VALID_MANIFEST);
        let reordered = parse(&reordered);
        let original_video = match &original.streams()[0].qualities()[0] {
            smooth_streaming_manifest_core::SmoothQualityLevel::Video(value) => value,
            _ => panic!("video"),
        };
        let reordered_video = match &reordered.streams()[0].qualities()[0] {
            smooth_streaming_manifest_core::SmoothQualityLevel::Video(value) => value,
            _ => panic!("video"),
        };
        let original_audio = match &original.streams()[1].qualities()[0] {
            smooth_streaming_manifest_core::SmoothQualityLevel::Audio(value) => value,
            _ => panic!("audio"),
        };
        let reordered_audio = match &reordered.streams()[1].qualities()[0] {
            smooth_streaming_manifest_core::SmoothQualityLevel::Audio(value) => value,
            _ => panic!("audio"),
        };

        let original_video_key = canonical_video_key(original_video).expect("video key framing");
        let reordered_video_key =
            canonical_video_key(reordered_video).expect("reordered video key framing");
        let original_audio_key =
            canonical_audio_key(original_audio, None).expect("audio key framing");
        let reordered_audio_key =
            canonical_audio_key(reordered_audio, None).expect("reordered audio key framing");

        assert_eq!(original_video_key, reordered_video_key);
        assert_eq!(original_audio_key, reordered_audio_key);
        assert!(original_video_key.starts_with("ss-v1-v-"));
        assert!(original_audio_key.starts_with("ss-v1-a-"));
        assert_eq!(original_video_key.len(), 72);
        assert_eq!(original_audio_key.len(), 72);
    }

    #[test]
    fn cancellation_collapses_before_partial_catalog_publication() {
        let manifest = Arc::new(parse(VALID_MANIFEST));
        let (identity, semantic) = catalog_identity();

        assert!(matches!(
            build_catalog(SmoothCatalogBuildRequest {
                manifest: &manifest,
                catalog_identity: identity,
                parent_semantic: &semantic,
                video_stream_ordinal: 0,
                audio_stream_ordinal: 1,
                preferred_height: PreferredHeightPolicy::NoPreference,
                policy: &policy(64 * 1024),
                cancellation: &|| true,
            }),
            Err(crate::SmoothPrepareError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_at_final_publication_fence_drops_complete_catalog() {
        let manifest = Arc::new(parse(VALID_MANIFEST));
        let policy = policy(64 * 1024);
        let successful_call_count = AtomicUsize::new(0);
        let (identity, semantic) = catalog_identity();
        build_catalog(SmoothCatalogBuildRequest {
            manifest: &manifest,
            catalog_identity: identity,
            parent_semantic: &semantic,
            video_stream_ordinal: 0,
            audio_stream_ordinal: 1,
            preferred_height: PreferredHeightPolicy::NoPreference,
            policy: &policy,
            cancellation: &|| {
                successful_call_count.fetch_add(1, Ordering::SeqCst);
                false
            },
        })
        .expect("baseline catalog");
        let total_successful_checks = successful_call_count.load(Ordering::SeqCst);
        let final_check_index = total_successful_checks
            .checked_sub(1)
            .expect("catalog обязан иметь final publication fence");

        let cancelled_call_count = AtomicUsize::new(0);
        let (identity, semantic) = catalog_identity();
        let result = build_catalog(SmoothCatalogBuildRequest {
            manifest: &manifest,
            catalog_identity: identity,
            parent_semantic: &semantic,
            video_stream_ordinal: 0,
            audio_stream_ordinal: 1,
            preferred_height: PreferredHeightPolicy::NoPreference,
            policy: &policy,
            cancellation: &|| {
                cancelled_call_count.fetch_add(1, Ordering::SeqCst) >= final_check_index
            },
        });

        assert!(matches!(result, Err(crate::SmoothPrepareError::Cancelled)));
        assert_eq!(
            cancelled_call_count.load(Ordering::SeqCst),
            total_successful_checks,
            "cancellation должна сработать только на последнем publication fence"
        );
    }
}
