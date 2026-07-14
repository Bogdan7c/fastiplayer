//! Immutable policy sibling-discovery без зависимости от config crate.

use crate::LocalMediaKind;

/// Persisted sibling filter vocabulary из D02.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SiblingFilter {
    /// Принимать только container-ы с video track.
    VideoOnly,
    /// Принимать обе media-категории.
    AllMedia,
    /// Принимать только audio-only container-ы.
    AudioOnly,
    /// Принимать ту же media-категорию, что и explicit target.
    #[default]
    SameAsOpened,
}

impl SiblingFilter {
    /// Проверяет только topology category; codec capability намеренно не участвует.
    #[must_use]
    pub fn admits(self, opened: LocalMediaKind, candidate: LocalMediaKind) -> bool {
        match self {
            Self::VideoOnly => matches!(candidate, LocalMediaKind::VideoContaining),
            Self::AllMedia => true,
            Self::AudioOnly => matches!(candidate, LocalMediaKind::AudioOnly),
            Self::SameAsOpened => opened == candidate,
        }
    }
}

/// Monotonic app-owned revision захваченной настройки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SiblingPolicyRevision(u64);

impl SiblingPolicyRevision {
    /// Создаёт revision из app-owned monotonic counter-а.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает opaque numeric revision для correlation diagnostics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// D62 snapshot: active job никогда не читает live config повторно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiblingDiscoveryPolicySnapshot {
    load_siblings: bool,
    filter: SiblingFilter,
    revision: SiblingPolicyRevision,
}

impl SiblingDiscoveryPolicySnapshot {
    /// Захватывает полную policy до manifest/probe scheduling.
    #[must_use]
    pub const fn new(
        load_siblings: bool,
        filter: SiblingFilter,
        revision: SiblingPolicyRevision,
    ) -> Self {
        Self {
            load_siblings,
            filter,
            revision,
        }
    }

    /// Сообщает, разрешал ли snapshot automatic siblings.
    #[must_use]
    pub const fn load_siblings(self) -> bool {
        self.load_siblings
    }

    /// Возвращает immutable filter текущего job-а.
    #[must_use]
    pub const fn filter(self) -> SiblingFilter {
        self.filter
    }

    /// Возвращает revision для app stale diagnostics.
    #[must_use]
    pub const fn revision(self) -> SiblingPolicyRevision {
        self.revision
    }
}
