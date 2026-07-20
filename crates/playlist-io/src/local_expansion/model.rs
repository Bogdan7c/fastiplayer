//! Public bounded model рекурсивного импорта локальных playlist-документов.

use std::fmt;
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use playlist_core::PlaylistSingleImportDraft;

use crate::{M3uImportIssueKind, M3uLineNumber, XspfGroup, XspfTrack};

/// Default maximum nesting depth, где root имеет depth `0`.
pub const DEFAULT_MAX_LOCAL_EXPANSION_DEPTH: usize = 8;
/// Default aggregate maximum числа открываемых playlist-документов.
pub const DEFAULT_MAX_LOCAL_EXPANSION_DOCUMENTS: usize = 256;
/// Default aggregate maximum принятых document bytes.
pub const DEFAULT_MAX_LOCAL_EXPANSION_BYTES: usize = 16 * 1024 * 1024;
/// Default aggregate maximum leaf items.
pub const DEFAULT_MAX_LOCAL_EXPANSION_ITEMS: usize = playlist_core::MAX_PLAYLIST_ITEMS;
/// Default maximum материализованных diagnostic details.
pub const DEFAULT_MAX_LOCAL_EXPANSION_DIAGNOSTICS: usize = 128;

/// Aggregate budgets одного recursive local expansion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalPlaylistExpansionLimits {
    /// Maximum nesting depth, где root имеет depth `0`.
    maximum_depth: usize,
    /// Maximum числа admitted local playlist documents.
    maximum_documents: usize,
    /// Maximum суммарных bytes успешно прочитанных документов.
    maximum_total_bytes: usize,
    /// Maximum leaf items во всём expansion tree.
    maximum_items: usize,
    /// Maximum retained diagnostic details; zero сохраняет только summary.
    maximum_diagnostics: usize,
}

impl LocalPlaylistExpansionLimits {
    /// Создаёт explicit aggregate profile без hidden config.
    pub const fn new(
        maximum_depth: usize,
        maximum_documents: usize,
        maximum_total_bytes: usize,
        maximum_items: usize,
        maximum_diagnostics: usize,
    ) -> Result<Self, LocalPlaylistExpansionLimitsError> {
        if maximum_documents == 0 {
            return Err(LocalPlaylistExpansionLimitsError::ZeroDocuments);
        }
        if maximum_total_bytes == 0 {
            return Err(LocalPlaylistExpansionLimitsError::ZeroTotalBytes);
        }
        if maximum_items == 0 {
            return Err(LocalPlaylistExpansionLimitsError::ZeroItems);
        }
        if maximum_items > playlist_core::MAX_PLAYLIST_ITEMS {
            return Err(
                LocalPlaylistExpansionLimitsError::ItemLimitExceedsDomainCapacity {
                    provided: maximum_items,
                    maximum: playlist_core::MAX_PLAYLIST_ITEMS,
                },
            );
        }

        Ok(Self {
            maximum_depth,
            maximum_documents,
            maximum_total_bytes,
            maximum_items,
            maximum_diagnostics,
        })
    }

    /// Возвращает maximum nesting depth.
    pub const fn maximum_depth(self) -> usize {
        self.maximum_depth
    }

    /// Возвращает document budget.
    pub const fn maximum_documents(self) -> usize {
        self.maximum_documents
    }

    /// Возвращает aggregate byte budget.
    pub const fn maximum_total_bytes(self) -> usize {
        self.maximum_total_bytes
    }

    /// Возвращает aggregate leaf-item budget.
    pub const fn maximum_items(self) -> usize {
        self.maximum_items
    }

    /// Возвращает retained diagnostic-detail budget.
    pub const fn maximum_diagnostics(self) -> usize {
        self.maximum_diagnostics
    }
}

impl Default for LocalPlaylistExpansionLimits {
    fn default() -> Self {
        Self {
            maximum_depth: DEFAULT_MAX_LOCAL_EXPANSION_DEPTH,
            maximum_documents: DEFAULT_MAX_LOCAL_EXPANSION_DOCUMENTS,
            maximum_total_bytes: DEFAULT_MAX_LOCAL_EXPANSION_BYTES,
            maximum_items: DEFAULT_MAX_LOCAL_EXPANSION_ITEMS,
            maximum_diagnostics: DEFAULT_MAX_LOCAL_EXPANSION_DIAGNOSTICS,
        }
    }
}

/// Ошибка сборки aggregate budget profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPlaylistExpansionLimitsError {
    /// Нельзя выполнить expansion без root document slot.
    ZeroDocuments,
    /// Нельзя прочитать ни одного complete документа.
    ZeroTotalBytes,
    /// Preview обязан иметь хотя бы один допустимый leaf slot.
    ZeroItems,
    /// Preview не может обещать больше domain capacity.
    ItemLimitExceedsDomainCapacity {
        /// Caller-provided item budget.
        provided: usize,
        /// Canonical playlist domain capacity.
        maximum: usize,
    },
}

impl fmt::Display for LocalPlaylistExpansionLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDocuments => formatter.write_str("document budget равен нулю"),
            Self::ZeroTotalBytes => formatter.write_str("aggregate byte budget равен нулю"),
            Self::ZeroItems => formatter.write_str("item budget равен нулю"),
            Self::ItemLimitExceedsDomainCapacity { provided, maximum } => write!(
                formatter,
                "item budget {provided} превышает domain capacity {maximum}"
            ),
        }
    }
}

impl std::error::Error for LocalPlaylistExpansionLimitsError {}

/// Per-request cooperative cancellation без глобальной generation state.
#[derive(Clone, Debug, Default)]
pub struct LocalPlaylistExpansionCancellation {
    /// Arc делает clone-ы одного request token-а общим first-writer state.
    cancelled: Arc<AtomicBool>,
}

impl LocalPlaylistExpansionCancellation {
    /// Создаёт независимый token; старый token не может отменить новый request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Публикует idempotent cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Проверяет cancellation на document boundary.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Поддержанный local playlist container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPlaylistDocumentFormat {
    /// Generic/HLS-classified `.m3u`.
    M3u,
    /// Generic/HLS-classified `.m3u8`.
    M3u8,
    /// Namespace-aware XSPF v1.
    Xspf,
}

/// Один document node сохраняет source boundaries и XSPF group indices.
#[derive(Clone, Debug)]
pub struct ExpandedLocalPlaylistDocument {
    /// Reversible original locator; canonical identity сюда не попадает.
    source_path: PathBuf,
    /// Declared container format.
    format: LocalPlaylistDocumentFormat,
    /// Source-order entries; include занимает исходную позицию одного entry/track.
    entries: Vec<ExpandedLocalPlaylistEntry>,
    /// Original XSPF group ranges; для M3U всегда empty.
    xspf_groups: Vec<XspfGroup>,
    /// False означает, что budget/cancellation остановили source entry traversal.
    source_complete: bool,
}

impl ExpandedLocalPlaylistDocument {
    /// Публикует fully processed document node одним commit-ом.
    pub(crate) fn new(
        source_path: PathBuf,
        format: LocalPlaylistDocumentFormat,
        entries: Vec<ExpandedLocalPlaylistEntry>,
        xspf_groups: Vec<XspfGroup>,
        source_complete: bool,
    ) -> Self {
        Self {
            source_path,
            format,
            entries,
            xspf_groups,
            source_complete,
        }
    }

    /// Возвращает exact reversible native locator.
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Возвращает declared container format.
    pub const fn format(&self) -> LocalPlaylistDocumentFormat {
        self.format
    }

    /// Возвращает source-order entries.
    pub fn entries(&self) -> &[ExpandedLocalPlaylistEntry] {
        &self.entries
    }

    /// Возвращает untouched XSPF source ranges.
    pub fn xspf_groups(&self) -> &[XspfGroup] {
        &self.xspf_groups
    }

    /// Итерирует leaf/failed-include entries в deterministic depth-first order.
    pub fn depth_first_entries(&self) -> DepthFirstExpandedEntries<'_> {
        DepthFirstExpandedEntries {
            pending_documents: vec![self.entries.iter()],
        }
    }

    /// Сообщает, представлены ли все source entries документа.
    pub const fn source_complete(&self) -> bool {
        self.source_complete
    }
}

/// Source-order node recursive expansion tree.
#[derive(Clone, Debug)]
pub enum ExpandedLocalPlaylistEntry {
    /// Generic M3U leaf уже имеет durable ID-less domain draft.
    M3uItem(Box<PlaylistSingleImportDraft>),
    /// XSPF leaf сохраняет ordered location candidates до S08 admission.
    XspfTrack(XspfTrack),
    /// Успешно раскрытый local playlist include.
    IncludedDocument(Box<ExpandedLocalPlaylistDocument>),
    /// Include не раскрыт из-за typed failure/budget/cancellation.
    UnexpandedInclude(UnexpandedLocalPlaylistInclude),
}

/// Borrowed DFS iterator не materialize-ит второй flattened preview.
pub struct DepthFirstExpandedEntries<'document> {
    /// Один source-order iterator на каждый active document depth.
    pending_documents: Vec<slice::Iter<'document, ExpandedLocalPlaylistEntry>>,
}

impl<'document> Iterator for DepthFirstExpandedEntries<'document> {
    type Item = &'document ExpandedLocalPlaylistEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let active_document = self.pending_documents.last_mut()?;
            match active_document.next() {
                Some(ExpandedLocalPlaylistEntry::IncludedDocument(document)) => {
                    self.pending_documents.push(document.entries.iter());
                }
                Some(entry) => return Some(entry),
                None => {
                    self.pending_documents.pop();
                }
            }
        }
    }
}

/// Original include payload для сохранения source cardinality после failure.
#[derive(Clone, Debug)]
pub enum UnexpandedLocalPlaylistInclude {
    /// M3U include сохраняет exact reversible draft locator.
    M3uItem(Box<PlaylistSingleImportDraft>),
    /// XSPF include сохраняет ordered alternatives и metadata.
    XspfTrack(XspfTrack),
}

/// Bounded diagnostic detail без раскрытия raw path/URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPlaylistExpansionIssue {
    /// Typed reason.
    kind: LocalPlaylistExpansionIssueKind,
    /// Document depth, на котором возникла проблема.
    document_depth: usize,
}

impl LocalPlaylistExpansionIssue {
    /// Создаёт safe diagnostic detail.
    pub(crate) const fn new(kind: LocalPlaylistExpansionIssueKind, document_depth: usize) -> Self {
        Self {
            kind,
            document_depth,
        }
    }

    /// Возвращает typed reason.
    pub const fn kind(&self) -> LocalPlaylistExpansionIssueKind {
        self.kind
    }

    /// Возвращает zero-based document depth.
    pub const fn document_depth(&self) -> usize {
        self.document_depth
    }
}

/// Typed recursive expansion diagnostic taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPlaylistExpansionIssueKind {
    /// Active DFS stack уже содержит canonical identity.
    CycleDetected,
    /// Native path не удалось canonicalize до cycle check.
    CanonicalizationFailed,
    /// Document не удалось открыть/прочитать.
    DocumentReadFailed,
    /// Include превысил maximum nesting depth.
    DepthBudgetExceeded,
    /// Include превысил aggregate document budget.
    DocumentBudgetExceeded,
    /// Complete document не помещается в remaining aggregate byte budget.
    ByteBudgetExceeded,
    /// Leaf item не помещается в aggregate item budget.
    ItemBudgetExceeded,
    /// Generic M3U parser завершился typed failure.
    M3uParseFailed,
    /// XSPF parser завершился typed failure.
    XspfParseFailed,
    /// Local HLS сознательно не поддерживается и не раскрывается как queue rows.
    LocalHlsUnsupported,
    /// M3U parser остановил document preview на собственном item cap.
    M3uDocumentItemLimitReached,
    /// Bounded M3U issue поднят в общий diagnostic stream.
    M3uImportIssue {
        /// Exact parser-owned issue kind.
        kind: M3uImportIssueKind,
        /// One-based source line.
        line: M3uLineNumber,
    },
}

/// Lossless counters сохраняются независимо от diagnostic detail cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalPlaylistExpansionSummary {
    /// Число document slots, занятых до filesystem read.
    pub(crate) documents_attempted: usize,
    /// Суммарные bytes complete документов, принятых aggregate budget-ом.
    pub(crate) document_bytes_read: usize,
    /// Число retained leaf items.
    pub(crate) retained_items: usize,
    /// Полное число diagnostics, включая omitted details.
    pub(crate) total_diagnostics: usize,
    /// Число omitted diagnostic details.
    pub(crate) omitted_diagnostics: usize,
    /// Число cycle rejections.
    pub(crate) cycle_rejections: usize,
    /// Число depth truncations.
    pub(crate) depth_truncations: usize,
    /// Число document-budget truncations.
    pub(crate) document_truncations: usize,
    /// Число byte-budget truncations.
    pub(crate) byte_truncations: usize,
    /// Число item-budget truncations.
    pub(crate) item_truncations: usize,
    /// Cancellation была замечена на document boundary.
    pub(crate) cancelled: bool,
}

impl LocalPlaylistExpansionSummary {
    /// Возвращает число consumed document slots.
    pub const fn documents_attempted(self) -> usize {
        self.documents_attempted
    }

    /// Возвращает accepted aggregate document bytes.
    pub const fn document_bytes_read(self) -> usize {
        self.document_bytes_read
    }

    /// Возвращает retained leaf count.
    pub const fn retained_items(self) -> usize {
        self.retained_items
    }

    /// Возвращает lossless diagnostic count.
    pub const fn total_diagnostics(self) -> usize {
        self.total_diagnostics
    }

    /// Возвращает число details, не materialized из-за diagnostic cap.
    pub const fn omitted_diagnostics(self) -> usize {
        self.omitted_diagnostics
    }

    /// Возвращает число detected active-stack cycles.
    pub const fn cycle_rejections(self) -> usize {
        self.cycle_rejections
    }

    /// Возвращает число depth truncations.
    pub const fn depth_truncations(self) -> usize {
        self.depth_truncations
    }

    /// Возвращает число document-budget truncations.
    pub const fn document_truncations(self) -> usize {
        self.document_truncations
    }

    /// Возвращает число byte-budget truncations.
    pub const fn byte_truncations(self) -> usize {
        self.byte_truncations
    }

    /// Возвращает число item-budget truncations.
    pub const fn item_truncations(self) -> usize {
        self.item_truncations
    }

    /// Сообщает, была ли замечена cancellation.
    pub const fn cancelled(self) -> bool {
        self.cancelled
    }

    /// Сообщает, был ли expansion усечён любым aggregate budget-ом.
    pub const fn was_truncated(self) -> bool {
        self.depth_truncations != 0
            || self.document_truncations != 0
            || self.byte_truncations != 0
            || self.item_truncations != 0
    }
}

/// Complete bounded result; root может отсутствовать при pre-parse failure/cancellation.
#[derive(Clone, Debug)]
pub struct LocalPlaylistExpansion {
    /// Root document tree, если root удалось разобрать.
    root_document: Option<ExpandedLocalPlaylistDocument>,
    /// Bounded safe diagnostic details.
    issues: Vec<LocalPlaylistExpansionIssue>,
    /// Lossless counters/truncation/cancellation summary.
    summary: LocalPlaylistExpansionSummary,
}

impl LocalPlaylistExpansion {
    /// Собирает final immutable result.
    pub(crate) fn new(
        root_document: Option<ExpandedLocalPlaylistDocument>,
        issues: Vec<LocalPlaylistExpansionIssue>,
        summary: LocalPlaylistExpansionSummary,
    ) -> Self {
        Self {
            root_document,
            issues,
            summary,
        }
    }

    /// Возвращает root expansion tree.
    pub const fn root_document(&self) -> Option<&ExpandedLocalPlaylistDocument> {
        self.root_document.as_ref()
    }

    /// Возвращает retained bounded diagnostics.
    pub fn issues(&self) -> &[LocalPlaylistExpansionIssue] {
        &self.issues
    }

    /// Возвращает lossless aggregate summary.
    pub const fn summary(&self) -> LocalPlaylistExpansionSummary {
        self.summary
    }
}

/// Invalid root request до начала filesystem traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalPlaylistExpansionStartError {
    /// Root extension не объявляет `.m3u`, `.m3u8` или `.xspf`.
    UnsupportedRootFormat,
    /// Relative root не даёт reversible authoritative base.
    RootPathMustBeAbsolute,
}

impl fmt::Display for LocalPlaylistExpansionStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRootFormat => {
                formatter.write_str("root document format не поддерживает local expansion")
            }
            Self::RootPathMustBeAbsolute => {
                formatter.write_str("root document path должен быть absolute")
            }
        }
    }
}

impl std::error::Error for LocalPlaylistExpansionStartError {}
