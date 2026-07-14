//! Публичная neutral vocabulary immutable directory manifest.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Exact D73 raw candidate limit, не являющийся пользовательским config.
pub const RAW_MANIFEST_MAX_ENTRIES: usize = 100_000;

/// Exact D73 native path + compact natural-key payload budget.
pub const RAW_MANIFEST_MAX_PATH_KEY_BYTES: usize = 64 * 1024 * 1024;

/// Stable job-local key, назначенный только после deterministic sort/dedup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManifestCandidateKey(u32);

impl ManifestCandidateKey {
    /// Возвращает opaque numeric value для bounded job-local maps Session 09A.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    pub(super) fn from_position(position: usize) -> Self {
        debug_assert!(position < RAW_MANIFEST_MAX_ENTRIES);
        Self(position as u32)
    }
}

/// Stable position record-а в полном natural manifest order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NaturalPosition(u32);

impl NaturalPosition {
    /// Возвращает zero-based natural position.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    pub(super) fn from_index(index: usize) -> Self {
        debug_assert!(index < RAW_MANIFEST_MAX_ENTRIES);
        Self(index as u32)
    }
}

/// Причина выбора durable presentation/open locator внутри alias group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasPresentationChoice {
    /// Группа содержит ровно один original entry.
    SoleEntry,
    /// Original path явного пользовательского target всегда побеждает.
    ExplicitTarget,
    /// Automatic группа предпочла direct non-symlink entry.
    DirectEntry,
    /// Direct entry отсутствовал; выбран deterministic natural/exact alias.
    DeterministicAlias,
}

/// Bounded diagnostics alias group без публикации transient canonical key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestAliasDiagnostics {
    original_entry_count: u32,
    presentation_choice: AliasPresentationChoice,
    canonicalization_failure: Option<io::ErrorKind>,
}

impl ManifestAliasDiagnostics {
    /// Число original directory aliases, объединённых canonical identity.
    #[must_use]
    pub const fn original_entry_count(&self) -> u32 {
        self.original_entry_count
    }

    /// Объясняет D45 selection без раскрытия resolved canonical path.
    #[must_use]
    pub const fn presentation_choice(&self) -> AliasPresentationChoice {
        self.presentation_choice
    }

    /// Canonicalization fallback остаётся typed и не исключает candidate.
    #[must_use]
    pub const fn canonicalization_failure(&self) -> Option<io::ErrorKind> {
        self.canonicalization_failure
    }

    pub(super) fn new(
        original_entry_count: usize,
        presentation_choice: AliasPresentationChoice,
        canonicalization_failure: Option<io::ErrorKind>,
    ) -> Self {
        Self {
            original_entry_count: original_entry_count as u32,
            presentation_choice,
            canonicalization_failure,
        }
    }
}

/// Probe/player/queue-neutral manifest record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestRecord {
    candidate_key: ManifestCandidateKey,
    original_locator: PathBuf,
    natural_position: NaturalPosition,
    alias_diagnostics: ManifestAliasDiagnostics,
}

impl ManifestRecord {
    /// Stable key действует только внутри одного immutable manifest.
    #[must_use]
    pub const fn candidate_key(&self) -> ManifestCandidateKey {
        self.candidate_key
    }

    /// Возвращает original presentation/open locator; canonical fallback отсутствует.
    #[must_use]
    pub fn original_locator(&self) -> &Path {
        &self.original_locator
    }

    /// Возвращает natural position до будущего scheduling/reprioritization.
    #[must_use]
    pub const fn natural_position(&self) -> NaturalPosition {
        self.natural_position
    }

    /// Возвращает bounded alias diagnostics.
    #[must_use]
    pub const fn alias_diagnostics(&self) -> &ManifestAliasDiagnostics {
        &self.alias_diagnostics
    }

    pub(super) fn new(
        position: usize,
        original_locator: PathBuf,
        alias_diagnostics: ManifestAliasDiagnostics,
    ) -> Self {
        Self {
            candidate_key: ManifestCandidateKey::from_position(position),
            original_locator,
            natural_position: NaturalPosition::from_index(position),
            alias_diagnostics,
        }
    }
}

/// Typed diagnostics для entries, которые нельзя безопасно классифицировать.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryManifestDiagnostic {
    /// Entry исчез или стал недоступен между `read_dir` и inspection.
    EntryInspectionFailed {
        original_locator: PathBuf,
        error_kind: io::ErrorKind,
    },
    /// Сам `read_dir` yielded ошибку без надёжного original path.
    EntryReadFailed { error_kind: io::ErrorKind },
    /// Дополнительные I/O diagnostics не удерживаются без границы.
    AdditionalFailuresOmitted { count: usize },
}

/// Какая exact D73 граница остановила manifest до публикации prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawManifestLimit {
    EntryCount,
    PathKeyBytes,
    CheckedArithmetic,
}

/// Typed D73 overflow; partial buffers уничтожаются до возврата ошибки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("raw manifest limit {limit:?} reached; observed at least {observed_at_least}")]
pub struct RawManifestLimitReached {
    limit: RawManifestLimit,
    observed_at_least: usize,
}

impl RawManifestLimitReached {
    /// Возвращает класс достигнутой границы.
    #[must_use]
    pub const fn limit(self) -> RawManifestLimit {
        self.limit
    }

    /// Возвращает безопасную нижнюю границу без зависимости от read_dir prefix.
    #[must_use]
    pub const fn observed_at_least(self) -> usize {
        self.observed_at_least
    }

    pub(super) const fn new(limit: RawManifestLimit, observed_at_least: usize) -> Self {
        Self {
            limit,
            observed_at_least,
        }
    }
}

/// Ошибка построения manifest до probe scheduling и queue mutation.
#[derive(Debug, Error)]
pub enum DirectoryManifestBuildError {
    #[error("explicit target has no filename or parent directory")]
    InvalidExplicitTarget,
    #[error("failed to resolve current directory: {0}")]
    CurrentDirectory(io::Error),
    #[error("failed to enumerate explicit target parent directory: {0}")]
    ReadParentDirectory(io::Error),
    #[error(transparent)]
    RawManifestLimitReached(#[from] RawManifestLimitReached),
}

/// Typed observation при targeted access к immutable snapshot record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateSourceDiagnostic {
    UnknownCandidateKey {
        candidate_key: ManifestCandidateKey,
    },
    MissingAfterSnapshot {
        candidate_key: ManifestCandidateKey,
    },
    SourceChangedAfterSnapshot {
        candidate_key: ManifestCandidateKey,
    },
    UnavailableAfterSnapshot {
        candidate_key: ManifestCandidateKey,
        error_kind: io::ErrorKind,
    },
}

impl fmt::Display for CandidateSourceDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCandidateKey { .. } => {
                formatter.write_str("candidate key does not belong to this manifest")
            }
            Self::MissingAfterSnapshot { .. } => {
                formatter.write_str("candidate source is missing after manifest snapshot")
            }
            Self::SourceChangedAfterSnapshot { .. } => {
                formatter.write_str("candidate source changed after manifest snapshot")
            }
            Self::UnavailableAfterSnapshot { error_kind, .. } => write!(
                formatter,
                "candidate source is unavailable after manifest snapshot: {error_kind:?}"
            ),
        }
    }
}
