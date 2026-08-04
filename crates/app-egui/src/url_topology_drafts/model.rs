//! Neutral app-owned values для topology draft preview и mapping algorithm.

use playlist_core::{MAX_PLAYLIST_ITEMS, PlaylistImportEntryDraft, PlaylistLocatorBuildError};
use service_ytdlp::YtDlpMediaLocator;
use thiserror::Error;

/// Максимум безопасно показываемых mapping issues в одном preview.
const MAX_YT_DLP_TOPOLOGY_DRAFT_ISSUES: usize = 256;

/// Чистый результат S16: drafts и bounded diagnostics, но никаких queue IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YtDlpTopologyDraftPreview {
    /// Ordered top-level Single/Compound drafts.
    pub(super) entries: Box<[PlaylistImportEntryDraft]>,
    /// Bounded safe issues для retained/ignored source nodes.
    pub(super) issues: Box<[YtDlpTopologyDraftIssue]>,
    /// Число дополнительных issues, не сохранённых из-за diagnostics budget.
    pub(super) omitted_issue_count: usize,
}

impl YtDlpTopologyDraftPreview {
    /// Итерирует top-level drafts в authoritative source order.
    pub(crate) fn entries(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistImportEntryDraft> + DoubleEndedIterator + '_ {
        self.entries.iter()
    }

    /// Возвращает retained Item demand без allocation.
    pub(crate) fn retained_item_count(&self) -> usize {
        self.entries
            .iter()
            .map(PlaylistImportEntryDraft::retained_item_count)
            .sum()
    }

    /// Итерирует safe issues в DFS source order.
    pub(crate) fn issues(
        &self,
    ) -> impl ExactSizeIterator<Item = &YtDlpTopologyDraftIssue> + DoubleEndedIterator + '_ {
        self.issues.iter()
    }

    /// Сообщает, сколько diagnostics не поместилось в bounded preview.
    pub(crate) const fn omitted_issue_count(&self) -> usize {
        self.omitted_issue_count
    }

    /// Передаёт owned drafts будущей staged import transaction без повторного mapping.
    pub(crate) fn into_entries(self) -> Vec<PlaylistImportEntryDraft> {
        self.entries.into_vec()
    }
}

/// Безопасная причина, по которой конкретный topology node не стал draft-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YtDlpTopologyDraftIssueKind {
    /// Source node не содержит устойчивой reopen identity.
    MissingStableIdentity,
    /// Service-owned stable identity не прошла neutral durable payload boundary.
    DurableIdentityRejected,
    /// Neutral Single payload отверг собственный bounded invariant.
    SingleDraftRejected,
    /// Multi-video не сохранил ни одной пригодной part.
    CompoundWithoutRetainedParts,
    /// Compound превысил hard part либо aggregate retained-item budget.
    CompoundPartLimitExceeded,
    /// Ordered top-level preview достиг hard retained-item budget.
    RetainedItemLimitExceeded,
}

/// Один redacted issue с one-based topology path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YtDlpTopologyDraftIssue {
    /// Typed safe category без locator/payload/error text.
    pub(super) kind: YtDlpTopologyDraftIssueKind,
    /// One-based child ordinals от root до проблемного node.
    pub(super) path: Box<[u32]>,
}

impl YtDlpTopologyDraftIssue {
    /// Возвращает safe category для preview/UI adapter-а.
    pub(crate) const fn kind(&self) -> YtDlpTopologyDraftIssueKind {
        self.kind
    }

    /// Возвращает source path; пустой path означает root.
    pub(crate) fn path(&self) -> &[u32] {
        &self.path
    }
}

/// Root-level mapping failure, при котором preview нельзя считать построенным.
#[derive(Debug, Error)]
pub(crate) enum YtDlpTopologyDraftMappingError {
    /// Уже service-classified exact root не удалось перенести в neutral URL locator.
    #[error("exact root locator не прошёл neutral playlist boundary")]
    RootLocator(#[from] PlaylistLocatorBuildError),
}

/// Локальные budgets позволяют тестировать truncation без огромных fixtures.
#[derive(Debug, Clone, Copy)]
pub(super) struct TopologyDraftMappingBudgets {
    /// Aggregate retained Item demand.
    pub(super) retained_items: usize,
    /// Сохранённые safe diagnostics.
    pub(super) issues: usize,
}

impl TopologyDraftMappingBudgets {
    /// Возвращает единственные production bounds для этого mapper-а.
    pub(super) const fn production() -> Self {
        Self {
            retained_items: MAX_PLAYLIST_ITEMS,
            issues: MAX_YT_DLP_TOPOLOGY_DRAFT_ISSUES,
        }
    }
}

/// Borrowed identity view скрывает storage service model от mapping algorithm.
#[derive(Clone, Copy)]
pub(super) struct TopologyIdentityView<'identity> {
    /// Extractor-local stable ID.
    pub(super) extractor_id: Option<&'identity str>,
    /// Extractor namespace/key для unambiguous reopen.
    pub(super) extractor_key: Option<&'identity str>,
    /// Stable webpage identity.
    pub(super) webpage_locator: Option<&'identity YtDlpMediaLocator>,
    /// Stable original identity.
    pub(super) original_locator: Option<&'identity YtDlpMediaLocator>,
}

/// Borrowed compact summary переносит только поля neutral playlist cache.
#[derive(Clone, Copy)]
pub(super) struct TopologySummaryView<'summary> {
    /// Bounded title.
    pub(super) title: Option<&'summary str>,
    /// Finite non-negative duration.
    pub(super) duration: Option<std::time::Duration>,
}

/// Intent-level node kinds, общие для production adapter-а и focused fake fixtures.
pub(super) enum TopologyNodeDescription<'node> {
    /// Самостоятельный playable video.
    Video {
        identity: TopologyIdentityView<'node>,
        metadata: TopologySummaryView<'node>,
    },
    /// Collection, чьи children flatten-ятся в текущий output scope.
    Collection,
    /// First-class compound root с ordered child parts.
    MultiVideo {
        identity: TopologyIdentityView<'node>,
        metadata: TopologySummaryView<'node>,
    },
    /// Leaf delegation; mapper не выполняет второй resolve.
    Delegation {
        target: &'node YtDlpMediaLocator,
        metadata: TopologySummaryView<'node>,
    },
    /// Retained unavailable child.
    Unavailable {
        identity: TopologyIdentityView<'node>,
        metadata: TopologySummaryView<'node>,
    },
}

/// Минимальный topology contract, на который опирается чистый mapping algorithm.
pub(super) trait TopologyMappingNode {
    /// Описывает текущий node через borrowed intent values.
    fn describe(&self) -> TopologyNodeDescription<'_>;

    /// Посещает authoritative ordered children без промежуточного `Vec`.
    fn visit_children(&self, visitor: &mut dyn FnMut(&Self));
}

impl std::fmt::Debug for TopologyIdentityView<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TopologyIdentityView")
            .field("has_extractor_id", &self.extractor_id.is_some())
            .field("has_extractor_key", &self.extractor_key.is_some())
            .field("has_webpage_locator", &self.webpage_locator.is_some())
            .field("has_original_locator", &self.original_locator.is_some())
            .finish()
    }
}
