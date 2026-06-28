//! Working set подготовленных кадров для timeline hover prepare.
//!
//! Модуль владеет только индексом, ключом, metadata и проверкой попадания.
//! Он не декодирует кадры, не хранит пиксели и не знает о player/render backend.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use media_core::{TrackDuration, TrackId, TrackTimestamp};
use video_present_core::{VideoFrameLease, VideoPresentFrameResourceDescriptor};

use crate::{BackendRevision, ScrubGenerationToken, ScrubTrackSelection, SourceRevision};

/// Политика точности именно для prepared working set-а.
///
/// Она намеренно отделена от `ScrubExactnessPolicy`: scheduler/scrub flow и
/// hover prepare cache могут развиваться разными темпами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameExactnessPolicy {
    /// Prepared frame подходит только если его фактический PTS не раньше target PTS.
    TargetOrAfter,
}

/// Bucket используется только как быстрый индекс и никогда не доказывает точность.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineHoverFrameBucket(i64);

impl TimelineHoverFrameBucket {
    /// Создаёт typed bucket id, который внешний owner вычисляет своей стратегией.
    #[must_use]
    pub const fn new(raw_bucket: i64) -> Self {
        Self(raw_bucket)
    }

    /// Возвращает raw bucket id для diagnostics/test assertions.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Полный ключ prepared entry-а внутри hover working set-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineHoverPrepareFrameKey {
    source_revision: SourceRevision,
    track_selection: ScrubTrackSelection,
    backend_revision: BackendRevision,
    hover_generation: ScrubGenerationToken,
    exactness_policy: FrameExactnessPolicy,
    target_bucket: TimelineHoverFrameBucket,
}

impl TimelineHoverPrepareFrameKey {
    /// Собирает ключ из typed guards без доступа к внутреннему storage working set-а.
    #[must_use]
    pub const fn new(
        source_revision: SourceRevision,
        track_selection: ScrubTrackSelection,
        backend_revision: BackendRevision,
        hover_generation: ScrubGenerationToken,
        exactness_policy: FrameExactnessPolicy,
        target_bucket: TimelineHoverFrameBucket,
    ) -> Self {
        Self {
            source_revision,
            track_selection,
            backend_revision,
            hover_generation,
            exactness_policy,
            target_bucket,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> SourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn track_selection(self) -> ScrubTrackSelection {
        self.track_selection
    }

    #[must_use]
    pub const fn backend_revision(self) -> BackendRevision {
        self.backend_revision
    }

    #[must_use]
    pub const fn hover_generation(self) -> ScrubGenerationToken {
        self.hover_generation
    }

    #[must_use]
    pub const fn exactness_policy(self) -> FrameExactnessPolicy {
        self.exactness_policy
    }

    #[must_use]
    pub const fn target_bucket(self) -> TimelineHoverFrameBucket {
        self.target_bucket
    }
}

/// Фактический timing prepared кадра.
///
/// `estimated_duration` хранится только как metadata. Validation V1 не использует
/// duration как доказательство exactness, потому что VFR/irregular timing может
/// сделать такой вывод неверным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineHoverPreparedFrameTiming {
    actual_pts: TrackTimestamp,
    estimated_duration: Option<TrackDuration>,
}

impl TimelineHoverPreparedFrameTiming {
    #[must_use]
    pub const fn new(actual_pts: TrackTimestamp) -> Self {
        Self {
            actual_pts,
            estimated_duration: None,
        }
    }

    #[must_use]
    pub const fn with_estimated_duration(mut self, estimated_duration: TrackDuration) -> Self {
        self.estimated_duration = Some(estimated_duration);
        self
    }

    #[must_use]
    pub const fn actual_pts(self) -> TrackTimestamp {
        self.actual_pts
    }

    #[must_use]
    pub const fn estimated_duration(self) -> Option<TrackDuration> {
        self.estimated_duration
    }
}

/// Provider-owned prepared entry: lease/resource handle + timing + opaque token.
pub struct TimelineHoverPreparedFrameEntry<BranchToken = ()> {
    lease: VideoFrameLease,
    timing: TimelineHoverPreparedFrameTiming,
    branch_token: Option<BranchToken>,
}

impl<BranchToken> TimelineHoverPreparedFrameEntry<BranchToken> {
    /// Создаёт entry без branch continuation token-а.
    #[must_use]
    pub const fn new(lease: VideoFrameLease, timing: TimelineHoverPreparedFrameTiming) -> Self {
        Self {
            lease,
            timing,
            branch_token: None,
        }
    }

    /// Добавляет provider-owned opaque token. Working set только хранит его.
    #[must_use]
    pub fn with_branch_token(mut self, branch_token: BranchToken) -> Self {
        self.branch_token = Some(branch_token);
        self
    }
}

/// Borrowed view на prepared frame, который прошёл key и timing validation.
pub struct TimelineHoverPreparedFrame<'a, BranchToken = ()> {
    key: TimelineHoverPrepareFrameKey,
    entry: &'a TimelineHoverPreparedFrameEntry<BranchToken>,
}

impl<'a, BranchToken> TimelineHoverPreparedFrame<'a, BranchToken> {
    #[must_use]
    pub const fn key(&self) -> TimelineHoverPrepareFrameKey {
        self.key
    }

    #[must_use]
    pub const fn lease(&self) -> &'a VideoFrameLease {
        &self.entry.lease
    }

    #[must_use]
    pub const fn timing(&self) -> TimelineHoverPreparedFrameTiming {
        self.entry.timing
    }

    #[must_use]
    pub fn resource_descriptor(&self) -> VideoPresentFrameResourceDescriptor {
        self.entry.lease.resource_descriptor()
    }

    #[must_use]
    pub const fn branch_token(&self) -> Option<&'a BranchToken> {
        self.entry.branch_token.as_ref()
    }
}

/// Owned entry, который уже вышел из hover-retained ownership.
///
/// S08B ещё не знает о реальном S17, поэтому тип только переносит lease и
/// branch token в нового владельца транзакции без копирования pixel payload.
pub struct TimelineHoverPromotedPreparedFrame<BranchToken = ()> {
    key: TimelineHoverPrepareFrameKey,
    entry: TimelineHoverPreparedFrameEntry<BranchToken>,
}

impl<BranchToken> TimelineHoverPromotedPreparedFrame<BranchToken> {
    fn from_validated_entry(
        key: TimelineHoverPrepareFrameKey,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) -> Self {
        Self { key, entry }
    }

    #[must_use]
    pub const fn key(&self) -> TimelineHoverPrepareFrameKey {
        self.key
    }

    #[must_use]
    pub const fn lease(&self) -> &VideoFrameLease {
        &self.entry.lease
    }

    #[must_use]
    pub const fn timing(&self) -> TimelineHoverPreparedFrameTiming {
        self.entry.timing
    }

    #[must_use]
    pub fn resource_descriptor(&self) -> VideoPresentFrameResourceDescriptor {
        self.entry.lease.resource_descriptor()
    }

    #[must_use]
    pub const fn branch_token(&self) -> Option<&BranchToken> {
        self.entry.branch_token.as_ref()
    }

    /// Показывает, можно ли использовать promoted entry как resume-ready branch.
    #[must_use]
    pub fn seek_reuse(&self) -> TimelineHoverPromotedFrameSeekReuse<'_, BranchToken> {
        match self.entry.branch_token.as_ref() {
            Some(branch_token) => {
                TimelineHoverPromotedFrameSeekReuse::ResumeReadyBranch { branch_token }
            }
            None => TimelineHoverPromotedFrameSeekReuse::VisualOverrideResumePending,
        }
    }
}

/// Как seek transaction может использовать promoted hover entry.
pub enum TimelineHoverPromotedFrameSeekReuse<'a, BranchToken> {
    /// Branch continuation token есть, значит owner может продолжать decode branch.
    ResumeReadyBranch { branch_token: &'a BranchToken },
    /// Есть только frame/lease: можно показать override и запускать resume_pending.
    VisualOverrideResumePending,
}

/// Lookup request: key выбирает candidate, target PTS доказывает exactness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineHoverPrepareFrameLookupRequest {
    key: TimelineHoverPrepareFrameKey,
    requested_target_pts: TrackTimestamp,
}

impl TimelineHoverPrepareFrameLookupRequest {
    #[must_use]
    pub const fn new(
        key: TimelineHoverPrepareFrameKey,
        requested_target_pts: TrackTimestamp,
    ) -> Self {
        Self {
            key,
            requested_target_pts,
        }
    }

    #[must_use]
    pub const fn key(self) -> TimelineHoverPrepareFrameKey {
        self.key
    }

    #[must_use]
    pub const fn requested_target_pts(self) -> TrackTimestamp {
        self.requested_target_pts
    }
}

/// Типизированный результат lookup-а без сведения miss/reject к `bool`.
pub enum TimelineHoverPrepareLookupOutcome<'a, BranchToken = ()> {
    Hit(TimelineHoverPreparedFrame<'a, BranchToken>),
    Miss(TimelineHoverPrepareLookupMissReason),
    TimingRejected(TimelineHoverPrepareTimingRejection),
}

/// Типизированный результат promotion-а без сведения miss/reject к `bool`.
pub enum TimelineHoverPreparePromotionOutcome<BranchToken = ()> {
    /// Entry содержит branch token и может перейти в resume-ready seek ownership.
    PromotedResumeReadyBranch(TimelineHoverPromotedPreparedFrame<BranchToken>),
    /// Entry содержит только frame lease и подходит только для override/resume_pending.
    PromotedVisualOverrideResumePending(TimelineHoverPromotedPreparedFrame<BranchToken>),
    Miss(TimelineHoverPrepareLookupMissReason),
    TimingRejected(TimelineHoverPrepareTimingRejection),
}

enum TimelineHoverPrepareLookupFailure {
    Miss(TimelineHoverPrepareLookupMissReason),
    TimingRejected(TimelineHoverPrepareTimingRejection),
}

/// Почему working set не нашёл запись с подходящим ключом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineHoverPrepareLookupMissReason {
    NoEntryForBucket {
        bucket: TimelineHoverFrameBucket,
    },
    NoEntryForKey,
    SourceRevisionMismatch {
        stored: SourceRevision,
        requested: SourceRevision,
    },
    BackendRevisionMismatch {
        stored: BackendRevision,
        requested: BackendRevision,
    },
    HoverGenerationMismatch {
        stored: ScrubGenerationToken,
        requested: ScrubGenerationToken,
    },
    TrackSelectionMismatch {
        stored: ScrubTrackSelection,
        requested: ScrubTrackSelection,
    },
    ExactnessPolicyMismatch {
        stored: FrameExactnessPolicy,
        requested: FrameExactnessPolicy,
    },
}

/// Почему найденная запись не прошла actual timing validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineHoverPrepareTimingRejection {
    RequestedTargetTrackMismatch {
        expected_video_track: TrackId,
        requested_track: TrackId,
    },
    ActualFrameTrackMismatch {
        expected_video_track: TrackId,
        actual_track: TrackId,
    },
    ActualFrameBeforeRequestedTarget {
        actual_pts: TrackTimestamp,
        requested_target_pts: TrackTimestamp,
    },
}

/// Bounded working set prepared кадров для timeline hover prepare.
pub struct TimelineHoverPrepareWorkingSet<BranchToken = ()> {
    capacity: NonZeroUsize,
    entries: HashMap<TimelineHoverPrepareFrameKey, TimelineHoverPreparedFrameEntry<BranchToken>>,
    insertion_order: VecDeque<TimelineHoverPrepareFrameKey>,
    bucket_index: HashMap<TimelineHoverFrameBucket, Vec<TimelineHoverPrepareFrameKey>>,
}

impl<BranchToken> TimelineHoverPrepareWorkingSet<BranchToken> {
    /// Создаёт generic working set с явной non-zero вместимостью.
    #[must_use]
    pub fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            bucket_index: HashMap::new(),
        }
    }

    #[must_use]
    pub const fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Вставляет prepared entry и evict-ит самые старые записи сверх capacity.
    ///
    /// Удаление entry просто drop-ает `VideoFrameLease`; release делает сам lease.
    pub fn insert_prepared_frame(
        &mut self,
        key: TimelineHoverPrepareFrameKey,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) {
        if self.entries.contains_key(&key) {
            self.remove_key_from_indexes(key);
        }

        self.entries.insert(key, entry);
        self.insertion_order.push_back(key);
        self.bucket_index
            .entry(key.target_bucket)
            .or_default()
            .push(key);

        self.evict_entries_over_capacity();
    }

    /// Ищет prepared frame: сначала bucket/key, затем actual timing validation.
    #[must_use]
    pub fn lookup_prepared_frame(
        &self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> TimelineHoverPrepareLookupOutcome<'_, BranchToken> {
        match self.find_validated_entry(&request) {
            Ok(entry) => TimelineHoverPrepareLookupOutcome::Hit(TimelineHoverPreparedFrame {
                key: request.key,
                entry,
            }),
            Err(TimelineHoverPrepareLookupFailure::Miss(reason)) => {
                TimelineHoverPrepareLookupOutcome::Miss(reason)
            }
            Err(TimelineHoverPrepareLookupFailure::TimingRejected(rejection)) => {
                TimelineHoverPrepareLookupOutcome::TimingRejected(rejection)
            }
        }
    }

    /// Валидирует и переносит prepared entry из hover ownership в seek ownership.
    ///
    /// Timing/key validation происходит до удаления: rejected candidate остаётся
    /// во владении hover working set-а и будет освобождён обычным hover cleanup.
    #[must_use]
    pub fn promote_prepared_frame(
        &mut self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> TimelineHoverPreparePromotionOutcome<BranchToken> {
        match self.find_validated_entry(&request) {
            Ok(_) => {}
            Err(TimelineHoverPrepareLookupFailure::Miss(reason)) => {
                return TimelineHoverPreparePromotionOutcome::Miss(reason);
            }
            Err(TimelineHoverPrepareLookupFailure::TimingRejected(rejection)) => {
                return TimelineHoverPreparePromotionOutcome::TimingRejected(rejection);
            }
        }

        let entry = self
            .entries
            .remove(&request.key)
            .expect("validated prepared entry must remain present until promotion removes it");
        self.remove_key_from_indexes(request.key);

        let promoted_frame =
            TimelineHoverPromotedPreparedFrame::from_validated_entry(request.key, entry);

        if promoted_frame.branch_token().is_some() {
            TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame)
        } else {
            TimelineHoverPreparePromotionOutcome::PromotedVisualOverrideResumePending(
                promoted_frame,
            )
        }
    }

    fn find_validated_entry(
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

        if let Some(rejection) = validate_entry_timing(&request, entry.timing) {
            return Err(TimelineHoverPrepareLookupFailure::TimingRejected(rejection));
        }

        Ok(entry)
    }

    fn evict_entries_over_capacity(&mut self) {
        while self.entries.len() > self.capacity.get() {
            let Some(oldest_key) = self.insertion_order.pop_front() else {
                break;
            };

            self.entries.remove(&oldest_key);
            self.remove_key_from_bucket_index(oldest_key);
        }
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
        let live_bucket_keys = bucket_keys
            .iter()
            .copied()
            .filter(|stored_key| self.entries.contains_key(stored_key));

        if let Some(stored_key) = live_bucket_keys.clone().find(|stored_key| {
            stored_key.source_revision != requested_key.source_revision
                && stored_key.backend_revision == requested_key.backend_revision
                && stored_key.hover_generation == requested_key.hover_generation
                && stored_key.track_selection == requested_key.track_selection
                && stored_key.exactness_policy == requested_key.exactness_policy
        }) {
            return TimelineHoverPrepareLookupMissReason::SourceRevisionMismatch {
                stored: stored_key.source_revision,
                requested: requested_key.source_revision,
            };
        }

        if let Some(stored_key) = live_bucket_keys.clone().find(|stored_key| {
            stored_key.source_revision == requested_key.source_revision
                && stored_key.backend_revision != requested_key.backend_revision
                && stored_key.hover_generation == requested_key.hover_generation
                && stored_key.track_selection == requested_key.track_selection
                && stored_key.exactness_policy == requested_key.exactness_policy
        }) {
            return TimelineHoverPrepareLookupMissReason::BackendRevisionMismatch {
                stored: stored_key.backend_revision,
                requested: requested_key.backend_revision,
            };
        }

        if let Some(stored_key) = live_bucket_keys.clone().find(|stored_key| {
            stored_key.source_revision == requested_key.source_revision
                && stored_key.backend_revision == requested_key.backend_revision
                && stored_key.hover_generation != requested_key.hover_generation
                && stored_key.track_selection == requested_key.track_selection
                && stored_key.exactness_policy == requested_key.exactness_policy
        }) {
            return TimelineHoverPrepareLookupMissReason::HoverGenerationMismatch {
                stored: stored_key.hover_generation,
                requested: requested_key.hover_generation,
            };
        }

        if let Some(stored_key) = live_bucket_keys.clone().find(|stored_key| {
            stored_key.source_revision == requested_key.source_revision
                && stored_key.backend_revision == requested_key.backend_revision
                && stored_key.hover_generation == requested_key.hover_generation
                && stored_key.track_selection != requested_key.track_selection
                && stored_key.exactness_policy == requested_key.exactness_policy
        }) {
            return TimelineHoverPrepareLookupMissReason::TrackSelectionMismatch {
                stored: stored_key.track_selection,
                requested: requested_key.track_selection,
            };
        }

        if let Some(stored_key) = live_bucket_keys.clone().find(|stored_key| {
            stored_key.source_revision == requested_key.source_revision
                && stored_key.backend_revision == requested_key.backend_revision
                && stored_key.hover_generation == requested_key.hover_generation
                && stored_key.track_selection == requested_key.track_selection
                && stored_key.exactness_policy != requested_key.exactness_policy
        }) {
            return TimelineHoverPrepareLookupMissReason::ExactnessPolicyMismatch {
                stored: stored_key.exactness_policy,
                requested: requested_key.exactness_policy,
            };
        }

        if live_bucket_keys.count() == 0 {
            return TimelineHoverPrepareLookupMissReason::NoEntryForKey;
        }

        TimelineHoverPrepareLookupMissReason::NoEntryForKey
    }
}

fn validate_entry_timing(
    request: &TimelineHoverPrepareFrameLookupRequest,
    timing: TimelineHoverPreparedFrameTiming,
) -> Option<TimelineHoverPrepareTimingRejection> {
    let expected_video_track = request.key.track_selection.video_track;

    if request.requested_target_pts.track_id != expected_video_track {
        return Some(
            TimelineHoverPrepareTimingRejection::RequestedTargetTrackMismatch {
                expected_video_track,
                requested_track: request.requested_target_pts.track_id,
            },
        );
    }

    if timing.actual_pts.track_id != expected_video_track {
        return Some(
            TimelineHoverPrepareTimingRejection::ActualFrameTrackMismatch {
                expected_video_track,
                actual_track: timing.actual_pts.track_id,
            },
        );
    }

    match request.key.exactness_policy {
        FrameExactnessPolicy::TargetOrAfter => {
            if timing
                .actual_pts
                .cmp_timeline_position(request.requested_target_pts)
                == Ordering::Less
            {
                return Some(
                    TimelineHoverPrepareTimingRejection::ActualFrameBeforeRequestedTarget {
                        actual_pts: timing.actual_pts,
                        requested_target_pts: request.requested_target_pts,
                    },
                );
            }
        }
    }

    None
}

impl TimelineHoverPrepareWorkingSet<()> {
    /// Создаёт обычный working set без branch-token payload-а.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self::with_capacity(capacity)
    }
}
