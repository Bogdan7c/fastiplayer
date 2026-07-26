//! HDS F4M fetch, hierarchy flattening, bootstrap resolution и quality policy.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use hds_manifest_core::{
    F4mBootstrapInfo, F4mBootstrapSource, F4mManifest, F4mMediaEntry, F4mStreamType,
    HdsBootstrapTimeline, parse_bootstrap, parse_f4m_manifest,
};
use source_core::HttpRequestTarget;
use url::Url;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication,
};
use web_media_core::{PreferredHeightPolicy, VideoHeight};

use crate::HdsVodOpenPolicy;

/// Refresh-stable provider identity rendition без locator/order/index material.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct HdsRenditionId(String);

impl HdsRenditionId {
    /// Создаёт identity только из canonical provider evidence.
    fn from_key(key: String) -> Self {
        Self(key)
    }

    /// Возвращает canonical key только provider-owned catalog mapper-у.
    pub(crate) fn as_key(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for HdsRenditionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HdsRenditionId")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Safe UI summary одной rendition без locator и authorization material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HdsRenditionSummary {
    /// Optional bitrate.
    pub bitrate: Option<u64>,
    /// Optional width.
    pub width: Option<u32>,
    /// Optional height.
    pub height: Option<u32>,
}

/// Безопасная provider-owned причина изоляции одной sibling rendition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdsRenditionRejectionReason {
    /// Parser отклонил только конкретную `<media>` row.
    MalformedManifestRow,
    /// Media/bootstrap locator конкретной row нельзя безопасно разрешить.
    InvalidLocator,
    /// Bootstrap отсутствует, недоступен или malformed.
    InvalidBootstrap,
    /// VOD duration нельзя доказать.
    MissingDuration,
    /// Две rows имеют один semantic contract и неразличимы без locator/order.
    AmbiguousSemanticIdentity,
    /// F4F bytes/container не прошли bounded demux probe.
    F4fProbeFailed,
    /// Demuxer опубликовал shape, отличный от ровно одного video + одного audio.
    UnsupportedTrackShape,
    /// Codec не входит в HDS profile или его descriptor нельзя выразить.
    UnsupportedCodec,
    /// Immutable decoder/renderer capability snapshot отклонил row.
    CapabilityUnavailable,
}

/// Одна bounded diagnostic row без locator, query или parser payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsRenditionRejection {
    reason: HdsRenditionRejectionReason,
}

impl HdsRenditionRejection {
    pub(crate) const fn new(reason: HdsRenditionRejectionReason) -> Self {
        Self { reason }
    }

    /// Возвращает безопасную typed причину.
    #[must_use]
    pub const fn reason(self) -> HdsRenditionRejectionReason {
        self.reason
    }
}

/// Selection intent provider-а: automatic quality сейчас и exact UI choice позже.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdsRenditionSelection {
    /// Выбирает rendition по глобальной neutral height policy и bitrate fallback.
    BestByPreference(PreferredHeightPolicy),
}

/// Internal resolved rendition с retained HTTP/bootstrap state.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedHdsRendition {
    /// Safe catalog identity.
    pub(crate) id: HdsRenditionId,
    /// Selected media base URL; actual F4F fragments append Seg/Frag suffix.
    pub(crate) media_target: HttpRequestTarget,
    /// Expanded ordered VOD timeline.
    pub(crate) timeline: HdsBootstrapTimeline,
    /// Manifest duration or timeline-derived duration.
    pub(crate) duration: Duration,
    /// Safe quality metadata.
    pub(crate) summary: HdsRenditionSummary,
}

/// Resolved root/child manifest set before selection.
pub(crate) struct ResolvedHdsPresentation {
    /// Flattened rendition rows.
    pub(crate) renditions: Vec<ResolvedHdsRendition>,
    /// Safe bounded sibling diagnostics.
    pub(crate) rejections: Vec<HdsRenditionRejection>,
}

/// Metadata inherited from a set-level hierarchy edge.
#[derive(Clone, Copy, Default)]
struct InheritedMetadata {
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<Duration>,
}

/// One pending manifest document in bounded DFS traversal.
struct PendingManifest {
    target: HttpRequestTarget,
    inherited: InheritedMetadata,
    depth: usize,
}

/// Fetches and resolves all approved VOD renditions from root F4M.
pub(crate) fn resolve_presentation(
    root_target: HttpRequestTarget,
    http: &AdaptiveHttpContext,
    policy: HdsVodOpenPolicy,
) -> Result<ResolvedHdsPresentation> {
    let mut pending = vec![PendingManifest {
        target: root_target,
        inherited: InheritedMetadata::default(),
        depth: 0,
    }];
    let mut visited = HashSet::new();
    let mut renditions = Vec::new();
    let mut rejections = Vec::new();
    let mut advertised_renditions = 0_usize;

    while let Some(node) = pending.pop() {
        if node.depth > policy.maximum_hierarchy_depth {
            bail!("HDS F4M hierarchy exceeds the configured depth");
        }
        let document_identity = node.target.expose_secret_for_request().to_owned();
        if visited.contains(&document_identity) {
            continue;
        }
        if visited.len() >= policy.maximum_manifest_documents {
            bail!("HDS manifest hierarchy exceeds the configured document limit");
        }
        visited.insert(document_identity);

        // Fetch/XML/schema/DRM/live document failures are presentation-fatal by contract.
        let fetched = fetch_manifest(http, node.target)?;
        let final_target = fetched.final_target().clone();
        let manifest =
            parse_f4m_manifest(fetched.bytes(), policy.xml_budgets, policy.manifest_limits)
                .with_context(|| "HDS F4M manifest parsing failed")?;
        if manifest.stream_type() == F4mStreamType::Live {
            bail!("HDS live manifest is outside approved S38 base/VOD profile");
        }
        rejections.extend(manifest.rejected_media().iter().map(|_| {
            HdsRenditionRejection::new(HdsRenditionRejectionReason::MalformedManifestRow)
        }));

        let base_target = resolve_base_target(&final_target, manifest.base_url())?;
        let manifest_metadata = InheritedMetadata {
            bitrate: node.inherited.bitrate,
            width: node.inherited.width,
            height: node.inherited.height,
            duration: manifest.duration().or(node.inherited.duration),
        };
        for media in manifest.media() {
            if let Some(href) = media.href() {
                let Ok(child_target) = base_target.resolve_reference(href) else {
                    rejections.push(HdsRenditionRejection::new(
                        HdsRenditionRejectionReason::InvalidLocator,
                    ));
                    continue;
                };
                pending.push(PendingManifest {
                    target: child_target,
                    inherited: merge_metadata(manifest_metadata, media),
                    depth: node.depth.saturating_add(1),
                });
                continue;
            }

            advertised_renditions = advertised_renditions
                .checked_add(1)
                .ok_or_else(|| anyhow!("HDS rendition count overflow"))?;
            if advertised_renditions > policy.maximum_renditions {
                bail!("HDS rendition count exceeds the configured limit");
            }
            let Some(media_url) = media.url() else {
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::InvalidLocator,
                ));
                continue;
            };
            let Ok(media_target) = base_target.resolve_reference(media_url) else {
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::InvalidLocator,
                ));
                continue;
            };
            let Ok(bootstrap) = select_bootstrap(&manifest, media) else {
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::InvalidBootstrap,
                ));
                continue;
            };
            let Ok(bootstrap_bytes) = fetch_bootstrap(http, &base_target, bootstrap, policy) else {
                if http.cancellation().is_cancelled() {
                    bail!("HDS rendition discovery was cancelled");
                }
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::InvalidBootstrap,
                ));
                continue;
            };
            let Ok(timeline) =
                parse_bootstrap(&bootstrap_bytes, media_url, policy.bootstrap_limits)
            else {
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::InvalidBootstrap,
                ));
                continue;
            };
            if timeline.live() {
                bail!("HDS bootstrap is live; S38 base card accepts VOD only");
            }
            let Some(duration) = manifest_metadata
                .duration
                .or_else(|| duration_from_timeline(&timeline))
            else {
                rejections.push(HdsRenditionRejection::new(
                    HdsRenditionRejectionReason::MissingDuration,
                ));
                continue;
            };
            let summary_without_id = (
                media.bitrate().or(manifest_metadata.bitrate),
                media.width().or(manifest_metadata.width),
                media.height().or(manifest_metadata.height),
            );
            let id = canonical_rendition_id(summary_without_id, duration, &timeline)?;
            let summary = HdsRenditionSummary {
                bitrate: summary_without_id.0,
                width: summary_without_id.1,
                height: summary_without_id.2,
            };
            renditions.push(ResolvedHdsRendition {
                id,
                media_target,
                timeline,
                duration,
                summary,
            });
        }
    }

    if renditions.is_empty() {
        bail!("HDS hierarchy contains no stream-level media rendition");
    }
    Ok(ResolvedHdsPresentation {
        renditions,
        rejections,
    })
}

/// Выбирает rendition по global policy или exact future UI identity.
pub(crate) fn select_rendition(
    presentation: ResolvedHdsPresentation,
    selection: HdsRenditionSelection,
) -> Result<ResolvedHdsRendition> {
    let selected_index = match selection {
        HdsRenditionSelection::BestByPreference(preference) => {
            let (index, best) = presentation
                .renditions
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| compare_renditions(left, right, preference))
                .ok_or_else(|| anyhow!("HDS rendition catalog is empty"))?;
            if presentation
                .renditions
                .iter()
                .enumerate()
                .any(|(other_index, other)| {
                    other_index != index && compare_renditions(best, other, preference).is_eq()
                })
            {
                bail!("HDS automatic rendition rank is ambiguous without locator/order tie-break");
            }
            index
        }
    };
    let mut renditions = presentation.renditions;
    Ok(renditions.swap_remove(selected_index))
}

/// Compares rendition quality without silently exposing URL identity.
fn compare_renditions(
    left: &ResolvedHdsRendition,
    right: &ResolvedHdsRendition,
    preference: PreferredHeightPolicy,
) -> std::cmp::Ordering {
    preference
        .compare(
            video_height(left.summary.height),
            video_height(right.summary.height),
        )
        .then_with(|| right.summary.bitrate.cmp(&left.summary.bitrate))
        .then_with(|| right.summary.height.cmp(&left.summary.height))
        .then_with(|| right.summary.width.cmp(&left.summary.width))
        .then_with(|| left.id.cmp(&right.id))
}

/// Строит versioned stable key без locator, query, hierarchy order и absolute origin.
fn canonical_rendition_id(
    summary: (Option<u64>, Option<u32>, Option<u32>),
    duration: Duration,
    timeline: &HdsBootstrapTimeline,
) -> Result<HdsRenditionId> {
    let fragment_count = timeline.fragments().len();
    Ok(HdsRenditionId::from_key(format!(
        "hds-v1-r-b{}-w{}-h{}-d{}.{:09}-t{}-n{}",
        optional_u64(summary.0),
        optional_u32(summary.1),
        optional_u32(summary.2),
        duration.as_secs(),
        duration.subsec_nanos(),
        timeline.timescale(),
        fragment_count,
    )))
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "x".to_owned(), |value| value.to_string())
}

fn optional_u32(value: Option<u32>) -> String {
    optional_u64(value.map(u64::from))
}

/// Builds checked neutral VideoHeight for global policy ranking.
fn video_height(height: Option<u32>) -> Option<VideoHeight> {
    height.and_then(|value| VideoHeight::new(value).ok())
}

/// Fetches one manifest through existing S31 bounded context.
fn fetch_manifest(
    http: &AdaptiveHttpContext,
    target: HttpRequestTarget,
) -> Result<web_media_adaptive::AdaptiveFetchedResource> {
    let request = AdaptiveResourceFetchRequest::full(
        http.source_generation(),
        target.clone(),
        http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::ApplyScopedReplacement,
    )
    .with_secret_forwarding(http.resource_secret_forwarding_for(&target));
    let fetched = http
        .fetch_resource_blocking(request)
        .map_err(|error| anyhow!("HDS manifest fetch failed: {error}"))?;
    Ok(fetched)
}

/// Resolves F4M baseURL against the redirect-effective manifest target.
fn resolve_base_target(
    manifest_target: &HttpRequestTarget,
    base_url: Option<&str>,
) -> Result<HttpRequestTarget> {
    match base_url {
        Some(base_url) => manifest_target
            .resolve_reference(base_url)
            .map_err(|_| anyhow!("HDS baseURL is invalid")),
        None => Ok(manifest_target.clone()),
    }
}

/// Selects bootstrapInfo by explicit id or the only unambiguous row.
fn select_bootstrap<'manifest>(
    manifest: &'manifest F4mManifest,
    media: &F4mMediaEntry,
) -> Result<&'manifest F4mBootstrapInfo> {
    if let Some(id) = media.bootstrap_info_id() {
        return manifest
            .bootstrap_info()
            .iter()
            .find(|bootstrap| bootstrap.id() == Some(id))
            .ok_or_else(|| anyhow!("HDS media references an absent bootstrapInfo id"));
    }
    if manifest.bootstrap_info().len() == 1 {
        return Ok(&manifest.bootstrap_info()[0]);
    }
    Err(anyhow!("HDS media bootstrapInfo is ambiguous"))
}

/// Fetches inline or URL bootstrap source while preserving S21 scope.
fn fetch_bootstrap(
    http: &AdaptiveHttpContext,
    base_target: &HttpRequestTarget,
    bootstrap: &F4mBootstrapInfo,
    policy: HdsVodOpenPolicy,
) -> Result<Vec<u8>> {
    match bootstrap.source() {
        F4mBootstrapSource::Inline(bytes) => Ok(bytes.to_vec()),
        F4mBootstrapSource::Url(url) => {
            let target = base_target
                .resolve_reference(url)
                .map_err(|_| anyhow!("HDS bootstrap URL is invalid"))?;
            let request = AdaptiveResourceFetchRequest::full(
                http.source_generation(),
                target.clone(),
                http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
                AdaptiveResourcePurpose::Manifest,
                AdaptiveResourceQueryApplication::ApplyScopedReplacement,
            )
            .with_secret_forwarding(http.resource_secret_forwarding_for(&target));
            let fetched = http
                .fetch_resource_blocking(request)
                .map_err(|error| anyhow!("HDS bootstrap fetch failed: {error}"))?;
            if fetched.bytes().len() > policy.bootstrap_limits.maximum_bytes.get() {
                bail!("HDS bootstrap exceeds the configured binary bound");
            }
            Ok(fetched.into_bytes())
        }
    }
}

/// Merges parent set-level quality metadata into child row.
fn merge_metadata(parent: InheritedMetadata, media: &F4mMediaEntry) -> InheritedMetadata {
    InheritedMetadata {
        bitrate: media.bitrate().or(parent.bitrate),
        width: media.width().or(parent.width),
        height: media.height().or(parent.height),
        duration: parent.duration,
    }
}

/// Derives VOD duration from the last finite fragment row.
fn duration_from_timeline(timeline: &HdsBootstrapTimeline) -> Option<Duration> {
    let first = timeline.fragments().first()?;
    let last = timeline.fragments().last()?;
    let end_units = last.timestamp().checked_add(u64::from(last.duration()))?;
    let duration_units = end_units.checked_sub(first.timestamp())?;
    Some(units_to_duration(duration_units, timeline.timescale()))
}

/// Converts timeline units without float rounding in the policy layer.
pub(crate) fn units_to_duration(units: u64, timescale: u32) -> Duration {
    let scale = u64::from(timescale);
    let seconds = units / scale;
    let remainder = units % scale;
    let nanos = u32::try_from((u128::from(remainder) * 1_000_000_000_u128) / u128::from(scale))
        .unwrap_or(u32::MAX);
    Duration::new(seconds, nanos)
}

/// Creates fragment URL by appending Adobe `SegN-FragM` path suffix.
pub(crate) fn fragment_target(
    media_target: &HttpRequestTarget,
    segment: u32,
    fragment: u32,
) -> Result<HttpRequestTarget> {
    let mut url = Url::parse(media_target.expose_secret_for_request())
        .map_err(|_| anyhow!("HDS media target cannot be parsed"))?;
    let path = format!("{}Seg{segment}-Frag{fragment}", url.path());
    url.set_path(&path);
    HttpRequestTarget::parse_exact(url.as_str())
        .map_err(|_| anyhow!("HDS fragment target cannot be represented"))
}

#[cfg(test)]
mod tests {
    use super::{
        HdsRenditionId, HdsRenditionSelection, HdsRenditionSummary, ResolvedHdsPresentation,
        ResolvedHdsRendition, duration_from_timeline, fragment_target, select_rendition,
    };
    use hds_manifest_core::{HdsBootstrapTimeline, HdsFragment};
    use source_core::HttpRequestTarget;
    use web_media_core::{PreferredHeightPolicy, PreferredVideoHeight};

    /// Проверяет, что global preferred height управляет automatic rendition selection.
    #[test]
    fn selects_preferred_height_before_bitrate_fallback() {
        let low = resolved_rendition(0, Some(720), Some(8_000_000));
        let preferred = resolved_rendition(1, Some(1_080), Some(5_000_000));
        let presentation = ResolvedHdsPresentation {
            renditions: vec![low, preferred],
            rejections: Vec::new(),
        };
        let preference = PreferredHeightPolicy::Prefer(
            PreferredVideoHeight::new(1_080).expect("valid preferred height"),
        );

        let selected = select_rendition(
            presentation,
            HdsRenditionSelection::BestByPreference(preference),
        )
        .expect("automatic HDS selection");

        assert_eq!(selected.id, rendition_id(1));
    }

    #[test]
    fn equal_semantic_rows_are_not_resolved_by_source_order() {
        let presentation = ResolvedHdsPresentation {
            renditions: vec![
                resolved_rendition(7, Some(720), Some(1_000_000)),
                resolved_rendition(7, Some(720), Some(1_000_000)),
            ],
            rejections: Vec::new(),
        };

        let error = select_rendition(
            presentation,
            HdsRenditionSelection::BestByPreference(PreferredHeightPolicy::NoPreference),
        )
        .expect_err("equal semantic rows must remain ambiguous");

        assert!(error.to_string().contains("ambiguous"));
    }

    /// Проверяет Adobe Seg/Frag suffix и сохранение scoped query parameters.
    #[test]
    fn fragment_target_preserves_query() {
        let media = HttpRequestTarget::parse_exact("https://media.example/video?token=secret")
            .expect("valid media target");

        let fragment = fragment_target(&media, 4, 17).expect("valid fragment target");

        assert_eq!(
            fragment.expose_secret_for_request(),
            "https://media.example/videoSeg4-Frag17?token=secret"
        );
    }

    /// Derived duration является длиной presentation, а не absolute end timestamp.
    #[test]
    fn timeline_duration_rebases_non_zero_fragment_origin() {
        let timeline = HdsBootstrapTimeline::from_parts(
            false,
            1_000,
            vec![
                HdsFragment::new(1, 9, 5_000, 1_000),
                HdsFragment::new(1, 10, 6_000, 1_000),
            ],
        );

        assert_eq!(
            duration_from_timeline(&timeline),
            Some(std::time::Duration::from_secs(2))
        );
    }

    /// Создаёт synthetic resolved row без network/authorization material.
    fn resolved_rendition(
        id: u32,
        height: Option<u32>,
        bitrate: Option<u64>,
    ) -> ResolvedHdsRendition {
        let timeline = HdsBootstrapTimeline::from_parts(false, 1_000, vec![]);
        let id = rendition_id(id);
        ResolvedHdsRendition {
            id,
            media_target: HttpRequestTarget::parse_exact("https://media.example/video")
                .expect("valid media target"),
            timeline,
            duration: std::time::Duration::from_secs(1),
            summary: HdsRenditionSummary {
                bitrate,
                width: None,
                height,
            },
        }
    }

    fn rendition_id(id: u32) -> HdsRenditionId {
        HdsRenditionId::from_key(format!("hds-test-{id}"))
    }
}
