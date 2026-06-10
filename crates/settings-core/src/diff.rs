use crate::metadata::{SettingId, SettingPath, SettingValue};

/// One changed setting in a document diff.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingChange {
    /// Stable setting id.
    pub id: SettingId,

    /// Stable document path for diagnostics.
    pub path: SettingPath,

    /// Baseline value.
    pub before: SettingValue,

    /// Current value.
    pub after: SettingValue,
}

impl SettingChange {
    /// Creates a changed-field record.
    #[must_use]
    pub fn new(
        id: SettingId,
        path: SettingPath,
        before: SettingValue,
        after: SettingValue,
    ) -> Self {
        Self {
            id,
            path,
            before,
            after,
        }
    }
}

/// Changed settings between two documents.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsDiff {
    changes: Vec<SettingChange>,
}

impl SettingsDiff {
    /// Creates an empty diff.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            changes: Vec::new(),
        }
    }

    /// Appends one changed-field record.
    pub fn push(&mut self, change: SettingChange) {
        self.changes.push(change);
    }

    /// Returns changed fields in registry order.
    #[must_use]
    pub fn changes(&self) -> &[SettingChange] {
        &self.changes
    }

    /// Consumes the diff and returns owned changed fields.
    #[must_use]
    pub fn into_changes(self) -> Vec<SettingChange> {
        self.changes
    }

    /// Returns true when there are no changed fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns the number of changed fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }
}
