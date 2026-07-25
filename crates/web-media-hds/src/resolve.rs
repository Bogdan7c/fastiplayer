//! HDS F4M fetch, hierarchy flattening, bootstrap resolution и quality policy.

use std::collections::HashSet;
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

/// Process-local identity одного flattened HDS rendition catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HdsRenditionId(u32);

impl HdsRenditionId {
    /// Создаёт catalog-local identity для будущего UI exact selection.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Возвращает numeric identity без URL/secret leakage.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Safe UI summary одной rendition без locator и authorization material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsRenditionSummary {
    /// Catalog-local selection identity.
    pub id: HdsRenditionId,
    /// Optional bitrate.
    pub bitrate: Option<u64>,
    /// Optional width.
    pub width: Option<u32>,
    /// Optional height.
    pub height: Option<u32>,
}

/// Immutable safe catalog, который позже можно передать UI stream picker-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdsRenditionCatalog {
    /// Bounded summaries в manifest order.
    rows: Box<[HdsRenditionSummary]>,
}

impl HdsRenditionCatalog {
    /// Возвращает safe rows без URL/secret state.
    #[must_use]
    pub fn rows(&self) -> &[HdsRenditionSummary] {
        &self.rows
    }
}

/// Selection intent provider-а: automatic quality сейчас и exact UI choice позже.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdsRenditionSelection {
    /// Выбирает rendition по глобальной neutral height policy и bitrate fallback.
    BestByPreference(PreferredHeightPolicy),
    /// Выбирает уже известную rendition из того же catalog snapshot-а.
    Exact(HdsRenditionId),
}

/// Internal resolved rendition с retained HTTP/bootstrap state.
#[derive(Debug)]
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
    /// Safe catalog projection for future UI.
    pub(crate) catalog: HdsRenditionCatalog,
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

    while let Some(node) = pending.pop() {
        if node.depth > policy.maximum_hierarchy_depth {
            bail!("HDS F4M hierarchy exceeds the configured depth");
        }
        if visited.insert(node.target.expose_secret_for_request().to_owned()) {
            let fetched = fetch_manifest(http, node.target)?;
            let final_target = fetched.final_target().clone();
            let manifest =
                parse_f4m_manifest(fetched.bytes(), policy.xml_budgets, policy.manifest_limits)
                    .with_context(|| "HDS F4M manifest parsing failed")?;
            if manifest.stream_type() == F4mStreamType::Live {
                bail!("HDS live manifest is outside approved S38 base/VOD profile");
            }
            let base_target = resolve_base_target(&final_target, manifest.base_url())?;
            let manifest_metadata = InheritedMetadata {
                bitrate: node.inherited.bitrate,
                width: node.inherited.width,
                height: node.inherited.height,
                duration: manifest.duration().or(node.inherited.duration),
            };
            for media in manifest.media() {
                if let Some(href) = media.href() {
                    let child_target = base_target
                        .resolve_reference(href)
                        .map_err(|_| anyhow!("HDS child manifest target is invalid"))?;
                    pending.push(PendingManifest {
                        target: child_target,
                        inherited: merge_metadata(manifest_metadata, media),
                        depth: node.depth.saturating_add(1),
                    });
                    continue;
                }
                let media_url = media
                    .url()
                    .ok_or_else(|| anyhow!("HDS media row has no URL or hierarchy href"))?;
                let media_target = base_target
                    .resolve_reference(media_url)
                    .map_err(|_| anyhow!("HDS media target is invalid"))?;
                let bootstrap = select_bootstrap(&manifest, media)?;
                let bootstrap_bytes = fetch_bootstrap(http, &base_target, bootstrap, policy)?;
                let timeline =
                    parse_bootstrap(&bootstrap_bytes, media_url, policy.bootstrap_limits)
                        .with_context(|| "HDS bootstrap timeline parsing failed")?;
                if timeline.live() {
                    bail!("HDS bootstrap is live; S38 base card accepts VOD only");
                }
                let duration = manifest_metadata
                    .duration
                    .or_else(|| duration_from_timeline(&timeline))
                    .ok_or_else(|| anyhow!("HDS VOD has no usable duration"))?;
                let id = HdsRenditionId::new(
                    u32::try_from(renditions.len())
                        .map_err(|_| anyhow!("HDS rendition identity exhausted"))?,
                );
                let summary = HdsRenditionSummary {
                    id,
                    bitrate: media.bitrate().or(manifest_metadata.bitrate),
                    width: media.width().or(manifest_metadata.width),
                    height: media.height().or(manifest_metadata.height),
                };
                renditions.push(ResolvedHdsRendition {
                    id,
                    media_target,
                    timeline,
                    duration,
                    summary,
                });
                if renditions.len() > policy.maximum_renditions {
                    bail!("HDS rendition count exceeds the configured limit");
                }
            }
        }
        if visited.len() > policy.maximum_manifest_documents {
            bail!("HDS manifest hierarchy exceeds the configured document limit");
        }
    }

    if renditions.is_empty() {
        bail!("HDS hierarchy contains no stream-level media rendition");
    }
    let catalog = HdsRenditionCatalog {
        rows: renditions
            .iter()
            .map(|row| row.summary)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    Ok(ResolvedHdsPresentation {
        renditions,
        catalog,
    })
}

/// Выбирает rendition по global policy или exact future UI identity.
pub(crate) fn select_rendition(
    presentation: ResolvedHdsPresentation,
    selection: HdsRenditionSelection,
) -> Result<(ResolvedHdsRendition, HdsRenditionCatalog)> {
    let selected_index = match selection {
        HdsRenditionSelection::Exact(id) => presentation
            .renditions
            .iter()
            .position(|row| row.id == id)
            .ok_or_else(|| anyhow!("HDS exact rendition is absent from this catalog snapshot"))?,
        HdsRenditionSelection::BestByPreference(preference) => presentation
            .renditions
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| compare_renditions(left, right, preference))
            .map(|(index, _)| index)
            .ok_or_else(|| anyhow!("HDS rendition catalog is empty"))?,
    };
    let mut renditions = presentation.renditions;
    let selected = renditions.swap_remove(selected_index);
    Ok((selected, presentation.catalog))
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
        .then_with(|| left.id.cmp(&right.id))
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
        HdsRenditionCatalog, HdsRenditionId, HdsRenditionSelection, HdsRenditionSummary,
        ResolvedHdsPresentation, ResolvedHdsRendition, duration_from_timeline, fragment_target,
        select_rendition,
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
            catalog: HdsRenditionCatalog {
                rows: vec![
                    HdsRenditionSummary {
                        id: HdsRenditionId::new(0),
                        bitrate: Some(8_000_000),
                        width: None,
                        height: Some(720),
                    },
                    HdsRenditionSummary {
                        id: HdsRenditionId::new(1),
                        bitrate: Some(5_000_000),
                        width: None,
                        height: Some(1_080),
                    },
                ]
                .into_boxed_slice(),
            },
        };
        let preference = PreferredHeightPolicy::Prefer(
            PreferredVideoHeight::new(1_080).expect("valid preferred height"),
        );

        let (selected, _) = select_rendition(
            presentation,
            HdsRenditionSelection::BestByPreference(preference),
        )
        .expect("automatic HDS selection");

        assert_eq!(selected.id, HdsRenditionId::new(1));
    }

    /// Проверяет exact selection contract для будущего UI picker-а.
    #[test]
    fn exact_selection_rejects_identity_from_another_snapshot() {
        let presentation = ResolvedHdsPresentation {
            renditions: vec![resolved_rendition(3, Some(720), Some(1_000_000))],
            catalog: HdsRenditionCatalog {
                rows: vec![HdsRenditionSummary {
                    id: HdsRenditionId::new(3),
                    bitrate: Some(1_000_000),
                    width: None,
                    height: Some(720),
                }]
                .into_boxed_slice(),
            },
        };

        let error = select_rendition(
            presentation,
            HdsRenditionSelection::Exact(HdsRenditionId::new(99)),
        )
        .expect_err("stale exact identity must fail closed");

        assert!(error.to_string().contains("absent"));
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
        ResolvedHdsRendition {
            id: HdsRenditionId::new(id),
            media_target: HttpRequestTarget::parse_exact("https://media.example/video")
                .expect("valid media target"),
            timeline,
            duration: std::time::Duration::from_secs(1),
            summary: HdsRenditionSummary {
                id: HdsRenditionId::new(id),
                bitrate,
                width: None,
                height,
            },
        }
    }
}
