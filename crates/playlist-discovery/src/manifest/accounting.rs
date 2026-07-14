//! Checked D73 accounting, изолированное от filesystem I/O и sorting.

use super::types::{
    RAW_MANIFEST_MAX_ENTRIES, RAW_MANIFEST_MAX_PATH_KEY_BYTES, RawManifestLimit,
    RawManifestLimitReached,
};

/// Internal limits injectable только для focused pure tests.
#[derive(Clone, Copy, Debug)]
pub(super) struct ManifestLimits {
    pub(super) max_entries: usize,
    pub(super) max_path_key_bytes: usize,
}

impl ManifestLimits {
    pub(super) const PRODUCTION: Self = Self {
        max_entries: RAW_MANIFEST_MAX_ENTRIES,
        max_path_key_bytes: RAW_MANIFEST_MAX_PATH_KEY_BYTES,
    };
}

/// Считает полный retained payload до возврата immutable manifest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RawManifestAccounting {
    entry_count: usize,
    path_key_bytes: usize,
}

impl RawManifestAccounting {
    /// Резервирует один original entry и его compact natural key атомарно.
    pub(super) fn add_entry(
        &mut self,
        path_bytes: usize,
        natural_key_bytes: usize,
        limits: ManifestLimits,
    ) -> Result<(), RawManifestLimitReached> {
        let next_entries = self.entry_count.checked_add(1).ok_or_else(|| {
            RawManifestLimitReached::new(RawManifestLimit::CheckedArithmetic, usize::MAX)
        })?;
        if next_entries > limits.max_entries {
            return Err(RawManifestLimitReached::new(
                RawManifestLimit::EntryCount,
                limits.max_entries.saturating_add(1),
            ));
        }

        let added_bytes = path_bytes.checked_add(natural_key_bytes).ok_or_else(|| {
            RawManifestLimitReached::new(RawManifestLimit::CheckedArithmetic, usize::MAX)
        })?;
        let next_bytes = self
            .path_key_bytes
            .checked_add(added_bytes)
            .ok_or_else(|| {
                RawManifestLimitReached::new(RawManifestLimit::CheckedArithmetic, usize::MAX)
            })?;
        if next_bytes > limits.max_path_key_bytes {
            return Err(RawManifestLimitReached::new(
                RawManifestLimit::PathKeyBytes,
                limits.max_path_key_bytes.saturating_add(1),
            ));
        }

        self.entry_count = next_entries;
        self.path_key_bytes = next_bytes;
        Ok(())
    }

    /// Учитывает один retained unique canonical identity path.
    pub(super) fn add_canonical_identity(
        &mut self,
        path_bytes: usize,
        limits: ManifestLimits,
    ) -> Result<(), RawManifestLimitReached> {
        let next_bytes = self.path_key_bytes.checked_add(path_bytes).ok_or_else(|| {
            RawManifestLimitReached::new(RawManifestLimit::CheckedArithmetic, usize::MAX)
        })?;
        if next_bytes > limits.max_path_key_bytes {
            return Err(RawManifestLimitReached::new(
                RawManifestLimit::PathKeyBytes,
                limits.max_path_key_bytes.saturating_add(1),
            ));
        }
        self.path_key_bytes = next_bytes;
        Ok(())
    }

    pub(super) const fn entry_count(self) -> usize {
        self.entry_count
    }

    pub(super) const fn path_key_bytes(self) -> usize {
        self.path_key_bytes
    }

    #[cfg(test)]
    pub(super) const fn with_state(entry_count: usize, path_key_bytes: usize) -> Self {
        Self {
            entry_count,
            path_key_bytes,
        }
    }
}
