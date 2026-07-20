use std::{fmt, num::NonZeroUsize};

/// One-based line identity для безопасной диагностики без raw input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct M3uLineNumber(NonZeroUsize);

impl M3uLineNumber {
    /// Строит line number из parser-owned one-based counter.
    pub(crate) fn from_one_based(line_number: usize) -> Self {
        Self(NonZeroUsize::new(line_number).expect("line counter is always one-based"))
    }

    /// Возвращает one-based значение для UI/report.
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Bounded recoverable issue generic M3U preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3uImportIssue {
    /// Строка, которой принадлежит issue.
    line: M3uLineNumber,
    /// Stable typed категория без raw locator/title.
    kind: M3uImportIssueKind,
}

impl M3uImportIssue {
    /// Создаёт issue внутри parser boundary.
    pub(crate) const fn new(line: M3uLineNumber, kind: M3uImportIssueKind) -> Self {
        Self { line, kind }
    }

    /// Возвращает one-based line identity.
    pub const fn line(&self) -> M3uLineNumber {
        self.line
    }

    /// Возвращает typed issue category.
    pub const fn kind(&self) -> M3uImportIssueKind {
        self.kind
    }
}

/// Recoverable generic M3U issue taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M3uImportIssueKind {
    /// Generic dialect принял и удалил UTF-8 BOM.
    Utf8BomIgnored,
    /// Строка превышает configured cap и была пропущена.
    LineLimitExceeded,
    /// EXTINF не соответствует declared dialect.
    MalformedExtInf,
    /// EXTINF не получил следующего locator.
    OrphanedExtInf,
    /// Опция/директива намеренно не исполняется.
    UnsupportedDirective,
    /// Locator похож на URI, но syntax/base resolution невалидны.
    MalformedLocator,
    /// Opaque/non-network scheme не является local/network draft.
    UnsupportedLocatorScheme,
    /// Locator не удалось перевести в bounded playlist-core draft.
    ImportDraftRejected,
    /// Item cap остановил дальнейшее materialization.
    ItemLimitExceeded,
}

/// Materialized issues плюс точное число не сохранённых из-за cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct M3uIssueSummary {
    /// Bounded issue storage.
    issues: Box<[M3uImportIssue]>,
    /// Число issues сверх caller budget.
    omitted_issue_count: usize,
}

impl M3uIssueSummary {
    /// Создаёт public immutable summary.
    pub(crate) fn new(issues: Vec<M3uImportIssue>, omitted_issue_count: usize) -> Self {
        Self {
            issues: issues.into_boxed_slice(),
            omitted_issue_count,
        }
    }

    /// Итерирует materialized issues без раскрытия storage.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &M3uImportIssue> + DoubleEndedIterator {
        self.issues.iter()
    }

    /// Возвращает число materialized issues.
    pub const fn retained_issue_count(&self) -> usize {
        self.issues.len()
    }

    /// Возвращает точное число omitted issues.
    pub const fn omitted_issue_count(&self) -> usize {
        self.omitted_issue_count
    }
}

/// Внутренний bounded collector.
pub(crate) struct IssueCollector {
    /// Caller-defined materialization cap.
    maximum_retained: usize,
    /// Retained typed issues.
    retained: Vec<M3uImportIssue>,
    /// Exact overflow accounting.
    omitted: usize,
}

impl IssueCollector {
    /// Создаёт collector с заранее ограниченной allocation.
    pub(crate) fn new(maximum_retained: usize) -> Self {
        Self {
            maximum_retained,
            retained: Vec::with_capacity(maximum_retained.min(32)),
            omitted: 0,
        }
    }

    /// Публикует issue либо увеличивает overflow counter.
    pub(crate) fn push(&mut self, line: M3uLineNumber, kind: M3uImportIssueKind) {
        if self.retained.len() < self.maximum_retained {
            self.retained.push(M3uImportIssue::new(line, kind));
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    /// Завершает collector без дополнительной копии.
    pub(crate) fn finish(self) -> M3uIssueSummary {
        M3uIssueSummary::new(self.retained, self.omitted)
    }
}

impl fmt::Display for M3uLineNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}
