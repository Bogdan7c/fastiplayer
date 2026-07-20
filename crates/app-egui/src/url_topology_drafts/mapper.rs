//! Pure ordered flatten/group mapping и service-owned durable identity encoding.

use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, MAX_PLAYLIST_IMPORT_COMPOUND_PARTS,
    PlaylistCompoundImportDraft, PlaylistImportAvailability, PlaylistImportEntryDraft,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistMediaKind,
    PlaylistPayloadBuildError, PlaylistSingleImportDraft, ServiceReopenMaterialKind,
};
use service_ytdlp::{
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
    YtDlpDurableReopenClassificationError, YtDlpDurableReopenIdentityInput,
    YtDlpDurableReopenMaterialKind, YtDlpDurableReopenPayload,
    classify_yt_dlp_delegation_reopen_target, classify_yt_dlp_durable_reopen_identity,
};

use super::{
    TopologyDraftMappingBudgets, TopologyIdentityView, TopologyMappingNode, TopologyMetadataView,
    TopologyNodeDescription, YtDlpTopologyDraftIssue, YtDlpTopologyDraftIssueKind,
    YtDlpTopologyDraftPreview,
};

/// Fallback, когда unavailable child не имеет даже display title.
const UNKNOWN_WEB_MEDIA_LABEL: &str = "Web media";
/// Fallback summary для редкого multi-video без usable root title.
const UNKNOWN_COMPOUND_MEDIA_LABEL: &str = "Compound web media";

/// Mutable accumulator остаётся единственным владельцем output allocations.
pub(super) struct TopologyDraftMapper {
    /// Exact user/root provenance для каждого созданного draft-а.
    durable_root_locator: DurableReopenLocator,
    /// Validated local limits.
    budgets: TopologyDraftMappingBudgets,
    /// Ordered top-level result.
    entries: Vec<PlaylistImportEntryDraft>,
    /// Safe bounded diagnostics.
    issues: Vec<YtDlpTopologyDraftIssue>,
    /// Число diagnostics за пределами budget.
    omitted_issue_count: usize,
    /// Текущий one-based DFS path.
    path: Vec<u32>,
    /// Aggregate retained Item demand уже принятых top-level drafts.
    retained_item_count: usize,
}

impl TopologyDraftMapper {
    /// Создаёт mapper без queue/player/runtime handles.
    fn new(
        durable_root_locator: DurableReopenLocator,
        budgets: TopologyDraftMappingBudgets,
    ) -> Self {
        Self {
            durable_root_locator,
            budgets,
            entries: Vec::new(),
            issues: Vec::new(),
            omitted_issue_count: 0,
            path: Vec::new(),
            retained_item_count: 0,
        }
    }

    /// Маппит root с exact-root reopen semantics.
    fn map_root<Node: TopologyMappingNode>(&mut self, root: &Node) {
        self.map_top_level_node(root, true);
    }

    /// Маппит node в top-level Single/Compound либо flatten-ит Collection.
    fn map_top_level_node<Node: TopologyMappingNode>(&mut self, node: &Node, is_root: bool) {
        match node.describe() {
            TopologyNodeDescription::Video { identity, metadata } => {
                let reopen_intent = self.reopen_intent(is_root, identity);
                self.push_top_level_single(
                    reopen_intent,
                    metadata,
                    PlaylistImportAvailability::Available,
                );
            }
            TopologyNodeDescription::Collection => {
                self.visit_children(node, |mapper, child| {
                    mapper.map_top_level_node(child, false);
                });
            }
            TopologyNodeDescription::MultiVideo { identity, metadata } => {
                self.push_compound(node, is_root, identity, metadata);
            }
            TopologyNodeDescription::Delegation { target, metadata } => {
                let reopen_intent = if is_root {
                    ReopenIntent::ExactRoot
                } else {
                    ReopenIntent::DelegationTarget(target)
                };
                self.push_top_level_single(
                    reopen_intent,
                    metadata,
                    PlaylistImportAvailability::Available,
                );
            }
            TopologyNodeDescription::Unavailable { identity, metadata } => {
                self.push_top_level_single(
                    ReopenIntent::ExtractedIdentity(identity),
                    metadata,
                    PlaylistImportAvailability::Unavailable,
                );
            }
        }
    }

    /// Строит first-class compound атомарно относительно mapper budgets.
    fn push_compound<Node: TopologyMappingNode>(
        &mut self,
        node: &Node,
        is_root: bool,
        identity: TopologyIdentityView<'_>,
        metadata: TopologyMetadataView<'_>,
    ) {
        let remaining_items = self
            .budgets
            .retained_items
            .saturating_sub(self.retained_item_count);
        let part_limit = remaining_items.min(MAX_PLAYLIST_IMPORT_COMPOUND_PARTS);
        let mut collector = CompoundPartCollector::new(part_limit);

        self.visit_children(node, |mapper, child| {
            mapper.collect_compound_parts(child, &mut collector);
        });

        if collector.overflowed {
            self.push_issue(YtDlpTopologyDraftIssueKind::CompoundPartLimitExceeded);
            return;
        }
        if collector.parts.is_empty() {
            self.push_issue(YtDlpTopologyDraftIssueKind::CompoundWithoutRetainedParts);
            return;
        }

        let reopen_locator = match self.build_reopen_locator(self.reopen_intent(is_root, identity))
        {
            Ok(locator) => locator,
            Err(kind) => {
                self.push_issue(kind);
                return;
            }
        };
        let summary = cached_metadata(metadata, UNKNOWN_COMPOUND_MEDIA_LABEL);
        let provenance = self.service_provenance();
        let retained_parts = collector.parts.len();

        match PlaylistCompoundImportDraft::new(reopen_locator, summary, provenance, collector.parts)
        {
            Ok(compound) => {
                self.retained_item_count += retained_parts;
                self.entries
                    .push(PlaylistImportEntryDraft::Compound(compound));
            }
            Err(_) => self.push_issue(YtDlpTopologyDraftIssueKind::CompoundPartLimitExceeded),
        }
    }

    /// Flatten-ит nested collections внутри compound, не создавая nested groups.
    fn collect_compound_parts<Node: TopologyMappingNode>(
        &mut self,
        node: &Node,
        collector: &mut CompoundPartCollector,
    ) {
        match node.describe() {
            TopologyNodeDescription::Video { identity, metadata } => {
                self.collect_compound_single(
                    collector,
                    ReopenIntent::ExtractedIdentity(identity),
                    metadata,
                    PlaylistImportAvailability::Available,
                );
            }
            TopologyNodeDescription::Collection | TopologyNodeDescription::MultiVideo { .. } => {
                self.visit_children(node, |mapper, child| {
                    mapper.collect_compound_parts(child, collector);
                });
            }
            TopologyNodeDescription::Delegation { target, metadata } => {
                self.collect_compound_single(
                    collector,
                    ReopenIntent::DelegationTarget(target),
                    metadata,
                    PlaylistImportAvailability::Available,
                );
            }
            TopologyNodeDescription::Unavailable { identity, metadata } => {
                self.collect_compound_single(
                    collector,
                    ReopenIntent::ExtractedIdentity(identity),
                    metadata,
                    PlaylistImportAvailability::Unavailable,
                );
            }
        }
    }

    /// Добавляет Single только если весь neutral payload прошёл owner invariants.
    fn push_top_level_single(
        &mut self,
        reopen_intent: ReopenIntent<'_>,
        metadata: TopologyMetadataView<'_>,
        availability: PlaylistImportAvailability,
    ) {
        if self.retained_item_count >= self.budgets.retained_items {
            self.push_issue(YtDlpTopologyDraftIssueKind::RetainedItemLimitExceeded);
            return;
        }

        match self.build_single(reopen_intent, metadata, availability) {
            Ok(single) => {
                self.retained_item_count += 1;
                self.entries.push(PlaylistImportEntryDraft::Single(single));
            }
            Err(kind) => self.push_issue(kind),
        }
    }

    /// Добавляет part в локальный compound collector; overflow помечает весь group rejected.
    fn collect_compound_single(
        &mut self,
        collector: &mut CompoundPartCollector,
        reopen_intent: ReopenIntent<'_>,
        metadata: TopologyMetadataView<'_>,
        availability: PlaylistImportAvailability,
    ) {
        if collector.parts.len() >= collector.part_limit {
            collector.overflowed = true;
            return;
        }

        match self.build_single(reopen_intent, metadata, availability) {
            Ok(single) => collector.parts.push(single),
            Err(kind) => self.push_issue(kind),
        }
    }

    /// Строит один ID-less Single с общим exact-root provenance.
    fn build_single(
        &self,
        reopen_intent: ReopenIntent<'_>,
        metadata: TopologyMetadataView<'_>,
        availability: PlaylistImportAvailability,
    ) -> Result<PlaylistSingleImportDraft, YtDlpTopologyDraftIssueKind> {
        let reopen_locator = self.build_reopen_locator(reopen_intent)?;
        let cached_metadata = cached_metadata(metadata, UNKNOWN_WEB_MEDIA_LABEL);
        let provenance = self.service_provenance();

        PlaylistSingleImportDraft::new(
            reopen_locator,
            cached_metadata,
            None,
            Vec::new(),
            provenance,
            availability,
        )
        .map_err(map_single_payload_error)
    }

    /// Выбирает exact root либо service-owned extracted identity без URL reparse.
    fn build_reopen_locator(
        &self,
        intent: ReopenIntent<'_>,
    ) -> Result<DurableReopenLocator, YtDlpTopologyDraftIssueKind> {
        match intent {
            ReopenIntent::ExactRoot => Ok(self.durable_root_locator.clone()),
            ReopenIntent::ExtractedIdentity(identity) => durable_extracted_identity(identity),
            ReopenIntent::DelegationTarget(target) => {
                let payload = classify_yt_dlp_delegation_reopen_target(target)
                    .map_err(map_durable_classification_error)?;
                durable_service_locator(payload)
            }
        }
    }

    /// Возвращает root/extracted reopen intent для текущего placement.
    fn reopen_intent<'identity>(
        &self,
        is_root: bool,
        identity: TopologyIdentityView<'identity>,
    ) -> ReopenIntent<'identity> {
        if is_root {
            ReopenIntent::ExactRoot
        } else {
            ReopenIntent::ExtractedIdentity(identity)
        }
    }

    /// Создаёт одинаковую durable root provenance для Single, part и Compound.
    fn service_provenance(&self) -> PlaylistImportProvenance {
        PlaylistImportProvenance::new(
            self.durable_root_locator.clone(),
            PlaylistImportSourceKind::Service,
            None,
        )
    }

    /// Обходит children и временно обновляет one-based diagnostics path.
    fn visit_children<Node: TopologyMappingNode>(
        &mut self,
        node: &Node,
        mut visitor: impl FnMut(&mut Self, &Node),
    ) {
        let mut child_index = 0usize;
        node.visit_children(&mut |child| {
            child_index += 1;
            // Service budget значительно меньше `u32::MAX`; saturation защищает test fakes.
            let path_ordinal = u32::try_from(child_index).unwrap_or(u32::MAX);
            self.path.push(path_ordinal);
            visitor(self, child);
            self.path.pop();
        });
    }

    /// Сохраняет issue либо увеличивает bounded overflow counter.
    fn push_issue(&mut self, kind: YtDlpTopologyDraftIssueKind) {
        if self.issues.len() < self.budgets.issues {
            self.issues.push(YtDlpTopologyDraftIssue {
                kind,
                path: self.path.clone().into_boxed_slice(),
            });
        } else {
            self.omitted_issue_count = self.omitted_issue_count.saturating_add(1);
        }
    }

    /// Завершает mapping и отдаёт только owned immutable preview values.
    fn finish(self) -> YtDlpTopologyDraftPreview {
        YtDlpTopologyDraftPreview {
            entries: self.entries.into_boxed_slice(),
            issues: self.issues.into_boxed_slice(),
            omitted_issue_count: self.omitted_issue_count,
        }
    }
}

/// Локальный atomic collector не позволяет публиковать усечённый compound.
struct CompoundPartCollector {
    /// Ordered retained parts.
    parts: Vec<PlaylistSingleImportDraft>,
    /// Совместный compound/global remaining limit.
    part_limit: usize,
    /// Любой overflow запрещает публикацию всей группы.
    overflowed: bool,
}

impl CompoundPartCollector {
    /// Создаёт пустой group accumulator.
    fn new(part_limit: usize) -> Self {
        Self {
            parts: Vec::new(),
            part_limit,
            overflowed: false,
        }
    }
}

/// Явно разделяет root, extracted child и delegation reopen semantics.
enum ReopenIntent<'identity> {
    /// Exact caller locator уже выбран app registry.
    ExactRoot,
    /// Stable child identity принадлежит service-ytdlp.
    ExtractedIdentity(TopologyIdentityView<'identity>),
    /// Delegation target остаётся leaf и не запускает второй extraction.
    DelegationTarget(&'identity service_ytdlp::YtDlpMediaLocator),
}

/// Запускает generic mapping algorithm над production либо focused fake node.
pub(super) fn map_topology_node<Node: TopologyMappingNode>(
    root: &Node,
    durable_root_locator: DurableReopenLocator,
    budgets: TopologyDraftMappingBudgets,
) -> YtDlpTopologyDraftPreview {
    let mut mapper = TopologyDraftMapper::new(durable_root_locator, budgets);
    mapper.map_root(root);
    mapper.finish()
}

/// Строит neutral cached metadata без service/runtime полей.
fn cached_metadata(
    metadata: TopologyMetadataView<'_>,
    fallback_label: &'static str,
) -> CachedPlaylistMetadata {
    let title = metadata.title.map(str::to_owned);
    let display_name = title.clone().unwrap_or_else(|| fallback_label.to_owned());
    let duration = metadata.duration.map(MediaDuration::from_duration);

    CachedPlaylistMetadata::new(display_name, PlaylistMediaKind::Video)
        .with_duration(duration)
        .with_title(title)
}

/// Приоритет identity: webpage, original, затем extractor key+ID.
fn durable_extracted_identity(
    identity: TopologyIdentityView<'_>,
) -> Result<DurableReopenLocator, YtDlpTopologyDraftIssueKind> {
    let payload = classify_yt_dlp_durable_reopen_identity(YtDlpDurableReopenIdentityInput {
        extractor_id: identity.extractor_id,
        extractor_key: identity.extractor_key,
        webpage_locator: identity.webpage_locator,
        original_locator: identity.original_locator,
    })
    .map_err(map_durable_classification_error)?;

    durable_service_locator(payload)
}

/// Единственная точка service material admission; ephemeral kinds здесь не создаются.
fn durable_service_locator(
    payload: YtDlpDurableReopenPayload,
) -> Result<DurableReopenLocator, YtDlpTopologyDraftIssueKind> {
    let material_kind = match payload.material_kind() {
        YtDlpDurableReopenMaterialKind::StableWebpageIdentity => {
            ServiceReopenMaterialKind::StableWebpageIdentity
        }
        YtDlpDurableReopenMaterialKind::StableOriginalIdentity => {
            ServiceReopenMaterialKind::StableOriginalIdentity
        }
        YtDlpDurableReopenMaterialKind::StableExtractorIdentity => {
            ServiceReopenMaterialKind::StableExtractorIdentity
        }
    };

    DurableReopenLocator::from_service_payload(
        YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
        YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
        material_kind,
        payload.into_payload_for_persistence(),
    )
    .map_err(|_| YtDlpTopologyDraftIssueKind::DurableIdentityRejected)
}

/// Сохраняет missing identity отдельной issue-категорией, остальные bounds объединяет.
fn map_durable_classification_error(
    error: YtDlpDurableReopenClassificationError,
) -> YtDlpTopologyDraftIssueKind {
    match error {
        YtDlpDurableReopenClassificationError::MissingStableIdentity => {
            YtDlpTopologyDraftIssueKind::MissingStableIdentity
        }
        YtDlpDurableReopenClassificationError::PayloadLimitExceeded { .. }
        | YtDlpDurableReopenClassificationError::ExtractorIdentityLengthExceeded => {
            YtDlpTopologyDraftIssueKind::DurableIdentityRejected
        }
    }
}

/// Не раскрывает внутренний neutral error: issue category достаточно для preview.
fn map_single_payload_error(_error: PlaylistPayloadBuildError) -> YtDlpTopologyDraftIssueKind {
    YtDlpTopologyDraftIssueKind::SingleDraftRejected
}
