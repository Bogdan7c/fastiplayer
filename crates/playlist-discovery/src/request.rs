//! Immutable request vocabulary и deterministic work-plan preparation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::frontier::SiblingAdmissionFrontier;
use crate::{
    AdmissionDirection, DirectoryManifest, DiscoveryJobKind, DiscoveryPriority, DiscoveryRecordKey,
    DiscoveryRequestRevision, LocalMediaFingerprint, LocalMediaKind, ManifestCandidateKey,
    SiblingDiscoveryPolicySnapshot,
};

/// Любой request обязан быть representable job-local `u32` key-ом и bounded memory.
pub const DISCOVERY_REQUEST_ITEM_LIMIT: usize = 100_000;

/// Immutable sibling request поверх готового D63 manifest-а.
pub struct SiblingDiscoveryRequest {
    manifest: Arc<DirectoryManifest>,
    opened_media_kind: LocalMediaKind,
    policy: SiblingDiscoveryPolicySnapshot,
    request_revision: DiscoveryRequestRevision,
}

impl SiblingDiscoveryRequest {
    /// Captures membership, filter и app correlation revision до scheduling.
    #[must_use]
    pub fn new(
        manifest: Arc<DirectoryManifest>,
        opened_media_kind: LocalMediaKind,
        policy: SiblingDiscoveryPolicySnapshot,
        request_revision: DiscoveryRequestRevision,
    ) -> Self {
        Self {
            manifest,
            opened_media_kind,
            policy,
            request_revision,
        }
    }
}

/// Один visible-row locator с optional fingerprint, который можно проверить без demux probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRefreshLocator {
    /// Exact native path; row identity остаётся у app owner-а.
    pub locator: PathBuf,
    /// Persisted/cache fingerprint; `None` требует полного metadata probe.
    pub expected_fingerprint: Option<LocalMediaFingerprint>,
}

impl VisibleRefreshLocator {
    /// Создаёт demand без filesystem I/O.
    #[must_use]
    pub fn new(locator: PathBuf, expected_fingerprint: Option<LocalMediaFingerprint>) -> Self {
        Self {
            locator,
            expected_fingerprint,
        }
    }
}

/// Один из reusable operation kinds поверх общего probe executor-а.
pub enum DiscoveryRequest {
    /// Automatic sibling discovery с D43/D74 frontier.
    Sibling(SiblingDiscoveryRequest),
    /// Explicit Add: caller применит successful subset одной domain mutation позже.
    ManualBatch {
        /// Immutable locators в caller-normalized deterministic order.
        locators: Vec<PathBuf>,
        /// App-owned structural/source revision.
        request_revision: DiscoveryRequestRevision,
    },
    /// Demand-driven visible/current metadata refresh.
    VisibleRefresh {
        /// Caller уже D31-deduplicated demands по своей row identity/revision.
        /// Одинаковые locators остаются разными ordinal-ами: path не является row ID.
        locators: Vec<VisibleRefreshLocator>,
        /// App-owned structural/source revision.
        request_revision: DiscoveryRequestRevision,
    },
    /// Full metadata preparation для последующего atomic Sort.
    MetadataSortPreparation {
        /// Exact locators с missing/stale metadata.
        locators: Vec<PathBuf>,
        /// App-owned structural/source revision.
        request_revision: DiscoveryRequestRevision,
    },
}

impl DiscoveryRequest {
    pub(crate) const fn kind(&self) -> DiscoveryJobKind {
        match self {
            Self::Sibling(_) => DiscoveryJobKind::SiblingDiscovery,
            Self::ManualBatch { .. } => DiscoveryJobKind::ManualBatch,
            Self::VisibleRefresh { .. } => DiscoveryJobKind::VisibleRefresh,
            Self::MetadataSortPreparation { .. } => DiscoveryJobKind::MetadataSortPreparation,
        }
    }

    pub(crate) const fn priority(&self) -> DiscoveryPriority {
        match self {
            Self::Sibling(_) | Self::ManualBatch { .. } => DiscoveryPriority::Foreground,
            Self::VisibleRefresh { .. } | Self::MetadataSortPreparation { .. } => {
                DiscoveryPriority::Speculative
            }
        }
    }

    pub(crate) const fn request_revision(&self) -> DiscoveryRequestRevision {
        match self {
            Self::Sibling(request) => request.request_revision,
            Self::ManualBatch {
                request_revision, ..
            }
            | Self::VisibleRefresh {
                request_revision, ..
            }
            | Self::MetadataSortPreparation {
                request_revision, ..
            } => *request_revision,
        }
    }

    pub(crate) fn item_count(&self) -> usize {
        match self {
            Self::Sibling(request) => request.manifest.records().len().saturating_sub(1),
            Self::ManualBatch { locators, .. } | Self::MetadataSortPreparation { locators, .. } => {
                locators.len()
            }
            Self::VisibleRefresh { locators, .. } => locators.len(),
        }
    }

    pub(crate) const fn outstanding_work_limit(&self) -> usize {
        match self {
            Self::ManualBatch { .. } => 1,
            Self::Sibling(_)
            | Self::VisibleRefresh { .. }
            | Self::MetadataSortPreparation { .. } => crate::PER_JOB_INPUT_LIMIT,
        }
    }

    pub(crate) fn speculative_execution_permit_limit(&self, general_worker_count: usize) -> usize {
        match self {
            Self::ManualBatch { .. } => 1,
            Self::Sibling(_)
            | Self::VisibleRefresh { .. }
            | Self::MetadataSortPreparation { .. } => general_worker_count.saturating_sub(1).max(1),
        }
    }
}

/// Neutral hint меняет только pending work order внутри immutable membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReprioritizeHint {
    pub(crate) preferred_keys: Box<[ManifestCandidateKey]>,
}

impl ReprioritizeHint {
    /// Ordered keys не несут repeat/shuffle semantics.
    #[must_use]
    pub fn new(preferred_keys: impl Into<Box<[ManifestCandidateKey]>>) -> Self {
        Self {
            preferred_keys: preferred_keys.into(),
        }
    }
}

/// Результат применения neutral priority hint-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReprioritizeOutcome {
    /// Pending keys, перемещённые в начало.
    pub reprioritized: usize,
    /// Unknown/already-started/terminal keys.
    pub stale: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkUnit {
    pub priority: DiscoveryPriority,
    pub direction: AdmissionDirection,
    pub directional_offset: usize,
    pub key: DiscoveryRecordKey,
    pub locator: PathBuf,
    pub expected_fingerprint: Option<LocalMediaFingerprint>,
}

pub(crate) struct WorkPlan {
    pub pending_work: VecDeque<WorkUnit>,
    pub frontier: Option<SiblingAdmissionFrontier>,
    pub manifest: Option<Arc<DirectoryManifest>>,
    pub policy: Option<SiblingDiscoveryPolicySnapshot>,
    pub opened_media_kind: Option<LocalMediaKind>,
}

pub(crate) fn build_work_plan(request: DiscoveryRequest) -> WorkPlan {
    match request {
        DiscoveryRequest::Sibling(request) => {
            if !request.policy.load_siblings() {
                return WorkPlan {
                    pending_work: VecDeque::new(),
                    frontier: Some(SiblingAdmissionFrontier::new(0, 0)),
                    manifest: Some(request.manifest),
                    policy: Some(request.policy),
                    opened_media_kind: Some(request.opened_media_kind),
                };
            }
            let target_index = request.manifest.explicit_target().candidate_key().get() as usize;
            let before_count = target_index;
            let after_count = request.manifest.records().len() - target_index - 1;
            let mut work = VecDeque::with_capacity(before_count + after_count);
            let maximum_offset = before_count.max(after_count);
            for offset in 0..maximum_offset {
                if offset < after_count {
                    let record = &request.manifest.records()[target_index + offset + 1];
                    work.push_back(work_from_manifest(
                        record.candidate_key(),
                        record.original_locator(),
                        AdmissionDirection::After,
                        offset,
                        if offset == 0 {
                            DiscoveryPriority::Foreground
                        } else {
                            DiscoveryPriority::Speculative
                        },
                    ));
                }
                if offset < before_count {
                    let record = &request.manifest.records()[target_index - offset - 1];
                    work.push_back(work_from_manifest(
                        record.candidate_key(),
                        record.original_locator(),
                        AdmissionDirection::Before,
                        offset,
                        DiscoveryPriority::Speculative,
                    ));
                }
            }
            WorkPlan {
                pending_work: work,
                frontier: Some(SiblingAdmissionFrontier::new(before_count, after_count)),
                manifest: Some(request.manifest),
                policy: Some(request.policy),
                opened_media_kind: Some(request.opened_media_kind),
            }
        }
        DiscoveryRequest::ManualBatch { locators, .. } => {
            let work = locators
                .into_iter()
                .enumerate()
                .map(|(index, locator)| WorkUnit {
                    priority: DiscoveryPriority::Foreground,
                    direction: AdmissionDirection::NonDirectional,
                    directional_offset: index,
                    key: DiscoveryRecordKey::Batch(checked_batch_key(index)),
                    locator,
                    expected_fingerprint: None,
                })
                .collect();
            WorkPlan {
                pending_work: work,
                frontier: None,
                manifest: None,
                policy: None,
                opened_media_kind: None,
            }
        }
        DiscoveryRequest::VisibleRefresh { locators, .. } => visible_refresh_work_plan(locators),
        DiscoveryRequest::MetadataSortPreparation { locators, .. } => {
            nondirectional_work_plan(locators, DiscoveryPriority::Speculative)
        }
    }
}

fn work_from_manifest(
    candidate_key: ManifestCandidateKey,
    locator: &Path,
    direction: AdmissionDirection,
    directional_offset: usize,
    priority: DiscoveryPriority,
) -> WorkUnit {
    WorkUnit {
        priority,
        direction,
        directional_offset,
        key: DiscoveryRecordKey::Manifest(candidate_key),
        locator: locator.to_path_buf(),
        expected_fingerprint: None,
    }
}

fn nondirectional_work_plan(locators: Vec<PathBuf>, priority: DiscoveryPriority) -> WorkPlan {
    let pending_work = locators
        .into_iter()
        .enumerate()
        .map(|(index, locator)| WorkUnit {
            priority,
            direction: AdmissionDirection::NonDirectional,
            directional_offset: index,
            key: DiscoveryRecordKey::Batch(checked_batch_key(index)),
            locator,
            expected_fingerprint: None,
        })
        .collect();
    WorkPlan {
        pending_work,
        frontier: None,
        manifest: None,
        policy: None,
        opened_media_kind: None,
    }
}

fn visible_refresh_work_plan(locators: Vec<VisibleRefreshLocator>) -> WorkPlan {
    let pending_work = locators
        .into_iter()
        .enumerate()
        .map(|(index, locator)| WorkUnit {
            priority: DiscoveryPriority::Speculative,
            direction: AdmissionDirection::NonDirectional,
            directional_offset: index,
            key: DiscoveryRecordKey::Batch(checked_batch_key(index)),
            locator: locator.locator,
            expected_fingerprint: locator.expected_fingerprint,
        })
        .collect();
    WorkPlan {
        pending_work,
        frontier: None,
        manifest: None,
        policy: None,
        opened_media_kind: None,
    }
}

fn checked_batch_key(index: usize) -> u32 {
    u32::try_from(index).expect("request item limit was validated before work-plan creation")
}
