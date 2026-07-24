//! Snapshot orchestration, selected-shape validation и Exact rematch identities.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use web_media_core::{
    CandidateDescriptor, CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration,
    SemanticIdentity, SourceIdentity, StreamLayout,
};

use crate::metadata::YtDlpPlaylistMetadata;

use super::descriptor::normalize_format_parts;
use super::model::{
    YtDlpCandidateComponentRole, YtDlpCandidateEntry, YtDlpCandidateNormalizationRejection,
    YtDlpCandidateOrigin, YtDlpCandidateSnapshot, YtDlpLiveIntent, YtDlpNormalizedCandidate,
    YtDlpRejectedCandidate, YtDlpSelectedCandidateShape,
};
use super::raw::{YtDlpCandidateDocument, YtDlpSerializedFormat};
use super::request_material::YtDlpRequestMaterial;

/// Нормализует один immutable extraction document.
pub(crate) fn normalize_candidate_document(
    document: YtDlpCandidateDocument,
    source: SourceIdentity,
    generation: ExtractionGeneration,
) -> YtDlpCandidateSnapshot {
    let live_intent = normalize_live_intent(document.is_live, document.live_status.as_deref());
    let playlist_metadata =
        YtDlpPlaylistMetadata::from_extractor_seconds(document.title, document.duration);
    let mut seen_format_identities = HashSet::new();
    let inventory = document
        .formats
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(ordinal, format)| {
            normalize_inventory_row(
                format,
                ordinal,
                source,
                generation,
                &mut seen_format_identities,
            )
        })
        .collect();
    let selected = normalize_selected_result(
        document.selected_format,
        document.requested_formats,
        source,
        generation,
    );

    YtDlpCandidateSnapshot::new(
        source,
        generation,
        playlist_metadata,
        live_intent,
        inventory,
        selected,
    )
}

/// Сводит official `is_live/live_status` fields в один fail-closed intent.
fn normalize_live_intent(is_live: Option<bool>, live_status: Option<&str>) -> YtDlpLiveIntent {
    use YtDlpLiveIntent::{Incompatible, Live, NotLive, PostLive, Unspecified, Upcoming};

    match (is_live, live_status) {
        (None, None) => Unspecified,
        (Some(true), None | Some("is_live")) | (None, Some("is_live")) => Live,
        (Some(false), None | Some("not_live" | "was_live"))
        | (None, Some("not_live" | "was_live")) => NotLive,
        (Some(false) | None, Some("is_upcoming")) => Upcoming,
        (Some(false) | None, Some("post_live")) => PostLive,
        _ => Incompatible,
    }
}

/// Сохраняет одну visible inventory row даже при rejection.
fn normalize_inventory_row(
    format: YtDlpSerializedFormat,
    ordinal: usize,
    source: SourceIdentity,
    generation: ExtractionGeneration,
    seen_format_identities: &mut HashSet<String>,
) -> YtDlpCandidateEntry {
    let origin = YtDlpCandidateOrigin::Inventory { ordinal };
    let Some(identity) = candidate_identity(&format, source, generation) else {
        return rejected(
            origin,
            None,
            YtDlpCandidateNormalizationRejection::InvalidFormatIdentity,
        );
    };
    let exact_format_identity = identity.format().as_str().to_owned();
    if !seen_format_identities.insert(exact_format_identity) {
        return rejected(
            origin,
            Some(identity),
            YtDlpCandidateNormalizationRejection::DuplicateFormatIdentity,
        );
    }

    normalize_single_candidate(format, identity, origin)
}

/// Нормализует selected result отдельно от inventory.
fn normalize_selected_result(
    selected_format: YtDlpSerializedFormat,
    requested_formats: Option<Vec<YtDlpSerializedFormat>>,
    source: SourceIdentity,
    generation: ExtractionGeneration,
) -> Option<YtDlpCandidateEntry> {
    let has_selected_identity = selected_format.format_id.is_some();
    let has_requested_components = requested_formats
        .as_ref()
        .is_some_and(|formats| !formats.is_empty());
    if !has_selected_identity && !has_requested_components {
        return None;
    }

    let selected_shape = if requested_formats
        .as_ref()
        .is_some_and(|formats| formats.len() == 2)
    {
        YtDlpSelectedCandidateShape::Compound
    } else {
        YtDlpSelectedCandidateShape::Single
    };
    let origin = YtDlpCandidateOrigin::Selected {
        shape: selected_shape,
    };
    let Some(identity) = candidate_identity(&selected_format, source, generation) else {
        return Some(rejected(
            origin,
            None,
            YtDlpCandidateNormalizationRejection::InvalidFormatIdentity,
        ));
    };

    match requested_formats {
        Some(components) if components.len() == 2 => {
            Some(normalize_compound_candidate(components, identity, origin))
        }
        Some(components) if components.len() > 2 => Some(rejected(
            origin,
            Some(identity),
            YtDlpCandidateNormalizationRejection::InvalidCompoundComponents,
        )),
        _ => Some(normalize_single_candidate(
            selected_format,
            identity,
            origin,
        )),
    }
}

/// Обычный selected/inventory format всегда остаётся единственным component-ом.
fn normalize_single_candidate(
    format: YtDlpSerializedFormat,
    identity: CandidateIdentity,
    origin: YtDlpCandidateOrigin,
) -> YtDlpCandidateEntry {
    match normalize_format_parts(&format).and_then(|parts| {
        let role = single_component_role(&parts.layout)?;
        build_candidate(identity.clone(), parts.layout, vec![(role, parts.request)])
    }) {
        Ok(candidate) => YtDlpCandidateEntry::accepted_entry(candidate),
        Err(reason) => rejected(origin, Some(identity), reason),
    }
}

/// Compound merge принимает только exact video-only + audio-only components.
fn normalize_compound_candidate(
    components: Vec<YtDlpSerializedFormat>,
    identity: CandidateIdentity,
    origin: YtDlpCandidateOrigin,
) -> YtDlpCandidateEntry {
    let component_identities = components
        .iter()
        .filter_map(|component| {
            component
                .format_id
                .clone()
                .and_then(|format_id| CandidateFormatIdentity::new(format_id).ok())
        })
        .collect::<Vec<_>>();
    if component_identities.len() != components.len()
        || component_identities[0] == component_identities[1]
    {
        return rejected(
            origin,
            Some(identity),
            YtDlpCandidateNormalizationRejection::InvalidCompoundComponents,
        );
    }

    let mut normalized_components = match components
        .iter()
        .map(normalize_format_parts)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(components) => components,
        Err(reason) => return rejected(origin, Some(identity), reason),
    };

    let mut video = None;
    let mut audio = None;
    let mut video_request_material = None;
    let mut audio_request_material = None;
    for component in normalized_components.drain(..) {
        match component.layout {
            StreamLayout::VideoOnly(video_component) if video.is_none() => {
                video = Some(video_component);
                video_request_material = Some(component.request);
            }
            StreamLayout::AudioOnly(audio_component) if audio.is_none() => {
                audio = Some(audio_component);
                audio_request_material = Some(component.request);
            }
            _ => {
                return rejected(
                    origin,
                    Some(identity),
                    YtDlpCandidateNormalizationRejection::InvalidCompoundComponents,
                );
            }
        }
    }

    let (Some(video), Some(audio), Some(video_request), Some(audio_request)) =
        (video, audio, video_request_material, audio_request_material)
    else {
        return rejected(
            origin,
            Some(identity),
            YtDlpCandidateNormalizationRejection::InvalidCompoundComponents,
        );
    };
    let layout = StreamLayout::Separate { video, audio };
    let request_material = vec![
        (YtDlpCandidateComponentRole::Video, video_request),
        (YtDlpCandidateComponentRole::Audio, audio_request),
    ];
    match build_candidate(identity.clone(), layout, request_material) {
        Ok(candidate) => YtDlpCandidateEntry::accepted_entry(candidate),
        Err(reason) => rejected(origin, Some(identity), reason),
    }
}

/// Создаёт exact snapshot identity без normalization format ID.
fn candidate_identity(
    format: &YtDlpSerializedFormat,
    source: SourceIdentity,
    generation: ExtractionGeneration,
) -> Option<CandidateIdentity> {
    let format_identity = CandidateFormatIdentity::new(format.format_id.clone()?).ok()?;
    Some(CandidateIdentity::new(source, generation, format_identity))
}

/// Строит semantic identity только из descriptor attributes, не request secrets.
fn build_candidate(
    identity: CandidateIdentity,
    layout: StreamLayout,
    request_material: Vec<(YtDlpCandidateComponentRole, YtDlpRequestMaterial)>,
) -> Result<YtDlpNormalizedCandidate, YtDlpCandidateNormalizationRejection> {
    let mut semantic_hasher = StableSemanticHasher::new();
    layout.hash(&mut semantic_hasher);
    let semantic_key = format!("yt-dlp-s19-v1-{:016x}", semantic_hasher.finish());
    let semantic_identity = SemanticIdentity::new(identity.source(), semantic_key)
        .map_err(|_| YtDlpCandidateNormalizationRejection::InvalidStreamLayout)?;
    let descriptor = CandidateDescriptor::new(identity, semantic_identity, layout, Vec::new())
        .map_err(|_| YtDlpCandidateNormalizationRejection::InvalidStreamLayout)?;
    Ok(YtDlpNormalizedCandidate::new(descriptor, request_material))
}

/// Выражает роль single resource через enum, а не positional convention.
fn single_component_role(
    layout: &StreamLayout,
) -> Result<YtDlpCandidateComponentRole, YtDlpCandidateNormalizationRejection> {
    match layout {
        StreamLayout::Muxed(_) => Ok(YtDlpCandidateComponentRole::Muxed),
        StreamLayout::VideoOnly(_) => Ok(YtDlpCandidateComponentRole::Video),
        StreamLayout::AudioOnly(_) => Ok(YtDlpCandidateComponentRole::Audio),
        StreamLayout::Separate { .. } => {
            Err(YtDlpCandidateNormalizationRejection::InvalidStreamLayout)
        }
    }
}

/// Stable FNV-1a hasher создаёт process-local semantic key.
///
/// Rematch дополнительно сравнивает полный layout, поэтому digest не является
/// единственным доказательством semantic equality.
struct StableSemanticHasher(u64);

impl StableSemanticHasher {
    /// FNV-1a offset basis.
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for StableSemanticHasher {
    /// Возвращает current digest.
    fn finish(&self) -> u64 {
        self.0
    }

    /// Добавляет bytes по fixed FNV-1a algorithm.
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Создаёт visible rejected entry.
const fn rejected(
    origin: YtDlpCandidateOrigin,
    identity: Option<CandidateIdentity>,
    reason: YtDlpCandidateNormalizationRejection,
) -> YtDlpCandidateEntry {
    YtDlpCandidateEntry::rejected_entry(YtDlpRejectedCandidate::new(origin, identity, reason))
}
