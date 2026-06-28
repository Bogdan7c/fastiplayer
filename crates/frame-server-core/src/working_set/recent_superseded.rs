use std::collections::{HashMap, VecDeque};

use video_present_core::{VideoPresentFrameResourceDescriptor, VideoPresentFrameResourceKind};

use super::{
    TimelineHoverFrameBucket, TimelineHoverPrepareFrameKey, TimelineHoverPrepareFrameLookupRequest,
    TimelineHoverPrepareLookupFailure, TimelineHoverPrepareLookupMissReason,
    TimelineHoverPreparedFrameEntry, classify_key_miss_for_live_bucket_keys, validate_entry_timing,
};
use crate::config::ValidatedFrameServerConfig;

/// Separate budget для pre-commit superseded click-back retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineHoverRecentSupersededBudget {
    general_slots: usize,
    software_slots: usize,
}

impl TimelineHoverRecentSupersededBudget {
    /// Отключает только click-back retention, не primary hover prepare.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            general_slots: 0,
            software_slots: 0,
        }
    }

    /// Создаёт budget из уже validated config slot counts.
    #[must_use]
    pub const fn from_validated_config(config: ValidatedFrameServerConfig) -> Self {
        Self {
            general_slots: config.recent_superseded_prepare_slots() as usize,
            software_slots: config.software_recent_superseded_prepare_slots() as usize,
        }
    }

    #[must_use]
    pub const fn general_slots(self) -> usize {
        self.general_slots
    }

    #[must_use]
    pub const fn software_slots(self) -> usize {
        self.software_slots
    }

    fn slots_for_path(self, resource_path: TimelineHoverRecentSupersededResourcePath) -> usize {
        match resource_path {
            TimelineHoverRecentSupersededResourcePath::General => self.general_slots,
            TimelineHoverRecentSupersededResourcePath::Software => self.software_slots,
        }
    }
}

/// Typed причина очистки recent compartment-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverRecentSupersededClearReason {
    GenerationChanged,
    RetentionDisabled,
    ResourcePressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TimelineHoverRecentSupersededResourcePath {
    General,
    Software,
}

impl TimelineHoverRecentSupersededResourcePath {
    fn from_descriptor(descriptor: VideoPresentFrameResourceDescriptor) -> Self {
        match descriptor.kind() {
            VideoPresentFrameResourceKind::HostPlanar => Self::Software,
            VideoPresentFrameResourceKind::DmaBufZeroCopy
            | VideoPresentFrameResourceKind::OpaqueBackendTexture
            | VideoPresentFrameResourceKind::ExternalGpuHandle => Self::General,
        }
    }
}

pub(super) struct TimelineHoverRecentSupersededEntries<BranchToken> {
    budget: TimelineHoverRecentSupersededBudget,
    entries: HashMap<TimelineHoverPrepareFrameKey, TimelineHoverPreparedFrameEntry<BranchToken>>,
    insertion_order: VecDeque<TimelineHoverPrepareFrameKey>,
    bucket_index: HashMap<TimelineHoverFrameBucket, Vec<TimelineHoverPrepareFrameKey>>,
}

impl<BranchToken> TimelineHoverRecentSupersededEntries<BranchToken> {
    pub(super) fn new(budget: TimelineHoverRecentSupersededBudget) -> Self {
        Self {
            budget,
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            bucket_index: HashMap::new(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn budget_for_descriptor(
        &self,
        descriptor: VideoPresentFrameResourceDescriptor,
    ) -> usize {
        self.budget
            .slots_for_path(TimelineHoverRecentSupersededResourcePath::from_descriptor(
                descriptor,
            ))
    }

    pub(super) fn insert_validated_demoted(
        &mut self,
        key: TimelineHoverPrepareFrameKey,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) {
        let resource_path =
            TimelineHoverRecentSupersededResourcePath::from_descriptor(entry.resource_descriptor());

        if self.entries.contains_key(&key) {
            self.remove_key_from_indexes(key);
        }

        self.entries.insert(key, entry);
        self.insertion_order.push_back(key);
        self.bucket_index
            .entry(key.target_bucket)
            .or_default()
            .push(key);

        self.evict_entries_over_path_budget(resource_path);
    }

    pub(super) fn find_validated_entry(
        &self,
        request: &TimelineHoverPrepareFrameLookupRequest,
    ) -> Result<&TimelineHoverPreparedFrameEntry<BranchToken>, TimelineHoverPrepareLookupFailure>
    {
        let entry = self.find_entry_for_request(request)?;

        if let Some(rejection) = validate_entry_timing(request, entry.timing) {
            return Err(TimelineHoverPrepareLookupFailure::TimingRejected(rejection));
        }

        Ok(entry)
    }

    pub(super) fn take_validated_entry(
        &mut self,
        request: &TimelineHoverPrepareFrameLookupRequest,
    ) -> Result<TimelineHoverPreparedFrameEntry<BranchToken>, TimelineHoverPrepareLookupFailure>
    {
        self.find_validated_entry(request)?;

        let entry = self
            .entries
            .remove(&request.key)
            .expect("validated recent entry must remain present until promotion removes it");
        self.remove_key_from_indexes(request.key);

        Ok(entry)
    }

    pub(super) fn clear(&mut self) -> usize {
        let cleared_entries = self.entries.len();
        self.entries.clear();
        self.insertion_order.clear();
        self.bucket_index.clear();
        cleared_entries
    }

    pub(super) fn remove_oldest_for_pressure(&mut self) -> Option<TimelineHoverPrepareFrameKey> {
        while let Some(oldest_key) = self.insertion_order.pop_front() {
            if self.entries.remove(&oldest_key).is_some() {
                self.remove_key_from_bucket_index(oldest_key);
                return Some(oldest_key);
            }
        }

        None
    }

    fn find_entry_for_request(
        &self,
        request: &TimelineHoverPrepareFrameLookupRequest,
    ) -> Result<&TimelineHoverPreparedFrameEntry<BranchToken>, TimelineHoverPrepareLookupFailure>
    {
        let bucket_keys = match self.bucket_index.get(&request.key.target_bucket) {
            Some(bucket_keys) if !bucket_keys.is_empty() => bucket_keys,
            _ => {
                return Err(TimelineHoverPrepareLookupFailure::Miss(
                    TimelineHoverPrepareLookupMissReason::NoEntryForBucket {
                        bucket: request.key.target_bucket,
                    },
                ));
            }
        };

        let Some(entry) = self.entries.get(&request.key) else {
            return Err(TimelineHoverPrepareLookupFailure::Miss(
                self.classify_key_miss(request.key, bucket_keys),
            ));
        };

        Ok(entry)
    }

    fn evict_entries_over_path_budget(
        &mut self,
        resource_path: TimelineHoverRecentSupersededResourcePath,
    ) {
        let budget = self.budget.slots_for_path(resource_path);

        while self.path_entry_count(resource_path) > budget {
            let Some(oldest_key) = self.oldest_key_for_path(resource_path) else {
                break;
            };

            self.entries.remove(&oldest_key);
            self.remove_key_from_indexes(oldest_key);
        }
    }

    fn path_entry_count(&self, resource_path: TimelineHoverRecentSupersededResourcePath) -> usize {
        self.entries
            .values()
            .filter(|entry| {
                TimelineHoverRecentSupersededResourcePath::from_descriptor(
                    entry.resource_descriptor(),
                ) == resource_path
            })
            .count()
    }

    fn oldest_key_for_path(
        &self,
        resource_path: TimelineHoverRecentSupersededResourcePath,
    ) -> Option<TimelineHoverPrepareFrameKey> {
        self.insertion_order.iter().copied().find(|key| {
            self.entries.get(key).is_some_and(|entry| {
                TimelineHoverRecentSupersededResourcePath::from_descriptor(
                    entry.resource_descriptor(),
                ) == resource_path
            })
        })
    }

    fn remove_key_from_indexes(&mut self, key: TimelineHoverPrepareFrameKey) {
        self.insertion_order.retain(|stored_key| *stored_key != key);
        self.remove_key_from_bucket_index(key);
    }

    fn remove_key_from_bucket_index(&mut self, key: TimelineHoverPrepareFrameKey) {
        let should_remove_bucket = match self.bucket_index.get_mut(&key.target_bucket) {
            Some(bucket_keys) => {
                bucket_keys.retain(|stored_key| *stored_key != key);
                bucket_keys.is_empty()
            }
            None => false,
        };

        if should_remove_bucket {
            self.bucket_index.remove(&key.target_bucket);
        }
    }

    fn classify_key_miss(
        &self,
        requested_key: TimelineHoverPrepareFrameKey,
        bucket_keys: &[TimelineHoverPrepareFrameKey],
    ) -> TimelineHoverPrepareLookupMissReason {
        classify_key_miss_for_live_bucket_keys(
            requested_key,
            bucket_keys
                .iter()
                .copied()
                .filter(|stored_key| self.entries.contains_key(stored_key)),
        )
    }
}

impl<BranchToken> TimelineHoverPreparedFrameEntry<BranchToken> {
    pub(super) fn resource_descriptor(&self) -> VideoPresentFrameResourceDescriptor {
        self.lease.resource_descriptor()
    }
}
