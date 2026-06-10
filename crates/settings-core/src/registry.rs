use std::collections::HashMap;

use crate::diff::{SettingChange, SettingsDiff};
use crate::error::{SettingsError, SettingsResult};
use crate::metadata::{DefaultBehavior, SettingAccess, SettingDescriptor, SettingId, SettingValue};

/// Storage boundary for one setting inside a project-specific document.
pub trait SettingAccessor<D>: Send + Sync {
    /// Reads the current setting value without exposing document storage.
    fn get(&self, document: &D) -> SettingsResult<SettingValue>;

    /// Writes the setting value with intent-level semantics.
    fn set(&self, document: &mut D, value: SettingValue) -> SettingsResult<()>;

    /// Resets the setting from the provided default document.
    fn reset(&self, document: &mut D, default_document: &D) -> SettingsResult<()>;
}

/// One registry entry: descriptor plus accessor boundary.
pub struct RegisteredSetting<D> {
    descriptor: SettingDescriptor,
    accessor: Box<dyn SettingAccessor<D>>,
}

impl<D> RegisteredSetting<D> {
    /// Creates a registry entry.
    #[must_use]
    pub fn new(descriptor: SettingDescriptor, accessor: impl SettingAccessor<D> + 'static) -> Self {
        Self {
            descriptor,
            accessor: Box::new(accessor),
        }
    }

    /// Returns descriptor metadata.
    #[must_use]
    pub fn descriptor(&self) -> &SettingDescriptor {
        &self.descriptor
    }
}

/// Project-agnostic settings metadata registry.
pub struct SettingsRegistry<D> {
    entries: Vec<RegisteredSetting<D>>,
    index_by_id: HashMap<SettingId, usize>,
}

impl<D> Default for SettingsRegistry<D> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<D> SettingsRegistry<D> {
    /// Creates an empty registry.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            index_by_id: HashMap::new(),
        }
    }

    /// Creates a registry from entries and rejects duplicate ids.
    pub fn new(entries: Vec<RegisteredSetting<D>>) -> SettingsResult<Self> {
        let mut registry = Self::empty();
        for entry in entries {
            registry.register_entry(entry)?;
        }
        Ok(registry)
    }

    /// Registers one setting and rejects duplicate ids.
    pub fn register(
        &mut self,
        descriptor: SettingDescriptor,
        accessor: impl SettingAccessor<D> + 'static,
    ) -> SettingsResult<()> {
        self.register_entry(RegisteredSetting::new(descriptor, accessor))
    }

    /// Registers a prebuilt setting entry.
    pub fn register_entry(&mut self, entry: RegisteredSetting<D>) -> SettingsResult<()> {
        let setting_id = entry.descriptor.id.clone();
        if self.index_by_id.contains_key(&setting_id) {
            return Err(SettingsError::DuplicateSettingId { id: setting_id });
        }

        let next_index = self.entries.len();
        self.entries.push(entry);
        self.index_by_id.insert(setting_id, next_index);
        Ok(())
    }

    /// Returns all descriptors in stable registry order.
    #[must_use]
    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = &SettingDescriptor> {
        self.entries.iter().map(RegisteredSetting::descriptor)
    }

    /// Looks up one descriptor by id.
    #[must_use]
    pub fn descriptor(&self, id: &SettingId) -> Option<&SettingDescriptor> {
        self.entry(id).map(RegisteredSetting::descriptor)
    }

    /// Reads a setting value through its accessor.
    pub fn get_value(&self, document: &D, id: &SettingId) -> SettingsResult<SettingValue> {
        let entry = self.required_entry(id)?;
        entry
            .accessor
            .get(document)
            .map_err(|error| error.with_setting_id(id))
    }

    /// Validates and writes a setting value through its accessor.
    pub fn set_value(
        &self,
        document: &mut D,
        id: &SettingId,
        value: SettingValue,
    ) -> SettingsResult<()> {
        let entry = self.required_entry(id)?;
        if entry.descriptor.access == SettingAccess::ReadOnly {
            return Err(SettingsError::ReadOnlySetting { id: id.clone() });
        }

        entry
            .descriptor
            .validate_value(&value)
            .map_err(|reason| SettingsError::InvalidValue {
                id: id.clone(),
                reason,
            })?;

        entry
            .accessor
            .set(document, value)
            .map_err(|error| error.with_setting_id(id))
    }

    /// Resets a setting through its accessor using a default document.
    pub fn reset_value(
        &self,
        document: &mut D,
        default_document: &D,
        id: &SettingId,
    ) -> SettingsResult<()> {
        let entry = self.required_entry(id)?;
        if entry.descriptor.access == SettingAccess::ReadOnly {
            return Err(SettingsError::ReadOnlySetting { id: id.clone() });
        }
        if entry.descriptor.default_behavior == DefaultBehavior::NoReset {
            return Err(SettingsError::ResetUnavailable { id: id.clone() });
        }

        entry
            .accessor
            .reset(document, default_document)
            .map_err(|error| error.with_setting_id(id))
    }

    /// Builds a changed-field diff by reading values through registered accessors.
    pub fn diff(&self, baseline: &D, current: &D) -> SettingsResult<SettingsDiff> {
        let mut diff = SettingsDiff::new();
        for entry in &self.entries {
            let setting_id = &entry.descriptor.id;
            let before = entry
                .accessor
                .get(baseline)
                .map_err(|error| error.with_setting_id(setting_id))?;
            let after = entry
                .accessor
                .get(current)
                .map_err(|error| error.with_setting_id(setting_id))?;

            if before != after {
                diff.push(SettingChange::new(
                    setting_id.clone(),
                    entry.descriptor.path.clone(),
                    before,
                    after,
                ));
            }
        }

        Ok(diff)
    }

    fn entry(&self, id: &SettingId) -> Option<&RegisteredSetting<D>> {
        self.index_by_id
            .get(id)
            .and_then(|entry_index| self.entries.get(*entry_index))
    }

    fn required_entry(&self, id: &SettingId) -> SettingsResult<&RegisteredSetting<D>> {
        self.entry(id)
            .ok_or_else(|| SettingsError::UnknownSetting { id: id.clone() })
    }
}
