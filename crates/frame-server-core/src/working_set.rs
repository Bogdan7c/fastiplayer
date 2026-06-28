//! Working set подготовленных кадров для timeline hover prepare.
//!
//! Модуль владеет только индексом, ключом, metadata и проверкой попадания.
//! Он не декодирует кадры, не хранит пиксели и не знает о player/render backend.

mod pressure;
mod recent_superseded;

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use media_core::{TrackDuration, TrackId, TrackTimestamp};
use video_present_core::{
    VideoFrameLease, VideoPresentFrameResourceDescriptor, VideoPresentFrameResourceKind,
};

pub use pressure::{
    TimelineHoverPrepareAdmissionMode, TimelineHoverPrepareAdmissionOutcome,
    TimelineHoverPrepareAdmissionRequest, TimelineHoverPrepareInsertOutcome,
    TimelineHoverPrepareNoOpReason, TimelineHoverPreparePressureReleaseMissReason,
    TimelineHoverPreparePressureReleaseOutcome, TimelineHoverPrepareProviderBudget,
    TimelineHoverPrepareSlotPlan,
};
pub use recent_superseded::{
    TimelineHoverRecentSupersededBudget, TimelineHoverRecentSupersededClearReason,
};

use self::pressure::{
    TimelineHoverPrepareAdmissionMode as AdmissionMode,
    TimelineHoverPrepareAdmissionOutcome as AdmissionOutcome,
    TimelineHoverPrepareNoOpReason as NoOpReason,
    TimelineHoverPreparePressureReleaseMissReason as PressureReleaseMissReason,
    TimelineHoverPreparePressureReleaseOutcome as PressureReleaseOutcome,
    TimelineHoverPrepareProviderBudget as ProviderBudget, TimelineHoverPrepareSlotPlan as SlotPlan,
};
use self::recent_superseded::TimelineHoverRecentSupersededEntries;
use crate::{
    BackendRevision, CancelScrubReason, ScrubGenerationToken, ScrubTrackSelection, SourceRevision,
};

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

    fn into_validated_entry(
        self,
    ) -> (
        TimelineHoverPrepareFrameKey,
        TimelineHoverPreparedFrameEntry<BranchToken>,
    ) {
        (self.key, self.entry)
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

/// Результат demote-back из seek transaction ownership в `recent_superseded`.
pub enum TimelineHoverPrepareDemoteBackOutcome<BranchToken = ()> {
    /// Promoted entry перешла в hover-owned recent compartment.
    DemotedToRecentSuperseded,
    /// Entry остаётся у transaction owner-а; caller должен завершить transaction release path.
    Rejected {
        promoted_frame: TimelineHoverPromotedPreparedFrame<BranchToken>,
        reason: TimelineHoverPrepareDemoteBackRejection,
    },
}

/// Почему promoted entry нельзя вернуть в `recent_superseded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineHoverPrepareDemoteBackRejection {
    CancelReasonDoesNotAllowDemote {
        actual: CancelScrubReason,
    },
    PromotedKeyNotCurrent {
        promoted_key: TimelineHoverPrepareFrameKey,
        current_key: TimelineHoverPrepareFrameKey,
    },
    TimingRejected(TimelineHoverPrepareTimingRejection),
    RecentSupersededRetentionDisabled {
        resource_kind: VideoPresentFrameResourceKind,
    },
}

enum TimelineHoverPrepareLookupFailure {
    Miss(TimelineHoverPrepareLookupMissReason),
    TimingRejected(TimelineHoverPrepareTimingRejection),
}

impl TimelineHoverPrepareLookupFailure {
    fn into_lookup_outcome<'a, BranchToken>(
        self,
    ) -> TimelineHoverPrepareLookupOutcome<'a, BranchToken> {
        match self {
            Self::Miss(reason) => TimelineHoverPrepareLookupOutcome::Miss(reason),
            Self::TimingRejected(rejection) => {
                TimelineHoverPrepareLookupOutcome::TimingRejected(rejection)
            }
        }
    }

    fn into_promotion_outcome<BranchToken>(
        self,
    ) -> TimelineHoverPreparePromotionOutcome<BranchToken> {
        match self {
            Self::Miss(reason) => TimelineHoverPreparePromotionOutcome::Miss(reason),
            Self::TimingRejected(rejection) => {
                TimelineHoverPreparePromotionOutcome::TimingRejected(rejection)
            }
        }
    }

    fn recent_fallback_or_primary(primary_failure: Self, recent_failure: Self) -> Self {
        match (primary_failure, recent_failure) {
            (Self::Miss(_), Self::TimingRejected(recent_rejection)) => {
                Self::TimingRejected(recent_rejection)
            }
            (primary_failure, _) => primary_failure,
        }
    }
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
    recent_superseded: TimelineHoverRecentSupersededEntries<BranchToken>,
}

impl<BranchToken> TimelineHoverPrepareWorkingSet<BranchToken> {
    /// Создаёт generic working set с явной non-zero вместимостью.
    #[must_use]
    pub fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self::with_capacity_and_recent_superseded(
            capacity,
            TimelineHoverRecentSupersededBudget::disabled(),
        )
    }

    /// Создаёт generic working set с отдельным budget-ом click-back retention.
    #[must_use]
    pub fn with_capacity_and_recent_superseded(
        capacity: NonZeroUsize,
        recent_superseded_budget: TimelineHoverRecentSupersededBudget,
    ) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            bucket_index: HashMap::new(),
            recent_superseded: TimelineHoverRecentSupersededEntries::new(recent_superseded_budget),
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

    #[must_use]
    pub fn recent_superseded_len(&self) -> usize {
        self.recent_superseded.len()
    }

    /// Dry-run admission перед запуском hover prepare.
    ///
    /// Метод не меняет storage и не освобождает lease. Caller использует его до
    /// дорогой подготовки, когда нужно понять: есть ли hover slot и provider pin.
    #[must_use]
    pub fn evaluate_prepare_admission(
        &self,
        request: TimelineHoverPrepareAdmissionRequest,
    ) -> TimelineHoverPrepareAdmissionOutcome {
        if request.mode() == AdmissionMode::ActiveLiveScrub {
            return AdmissionOutcome::NoOp {
                reason: NoOpReason::ActiveLiveScrubSuspendsHoverPrepare,
            };
        }

        if request.provider_budget() == ProviderBudget::ExhaustedAfterActivePins {
            return AdmissionOutcome::NoOp {
                reason: NoOpReason::ProviderResourcePressure,
            };
        }

        match self.slot_plan_for_admission(request) {
            Some(slot_plan) => AdmissionOutcome::Admitted { slot_plan },
            None => AdmissionOutcome::NoOp {
                reason: NoOpReason::NoSpareHoverSlot {
                    capacity: self.capacity.get(),
                    used_slots: self.entries.len(),
                    protected_key: request.protected_key(),
                },
            },
        }
    }

    /// Вставляет prepared entry только если admission всё ещё разрешён.
    ///
    /// При отказе entry возвращается caller-у, чтобы working set не делал
    /// скрытый release и не менял состояние при typed pressure/no-op.
    #[must_use]
    pub fn try_insert_prepared_frame(
        &mut self,
        request: TimelineHoverPrepareAdmissionRequest,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) -> TimelineHoverPrepareInsertOutcome<BranchToken> {
        let slot_plan = match self.evaluate_prepare_admission(request) {
            AdmissionOutcome::Admitted { slot_plan } => slot_plan,
            AdmissionOutcome::NoOp { reason } => {
                return TimelineHoverPrepareInsertOutcome::NoOp { entry, reason };
            }
        };

        self.insert_primary_entry(request.prepared_key(), entry);
        let evicted_primary_byproducts =
            self.evict_entries_over_capacity_protecting(request.protected_key());

        TimelineHoverPrepareInsertOutcome::Inserted {
            slot_plan,
            evicted_primary_byproducts,
        }
    }

    /// Освобождает один hover-owned entry под provider/resource pressure.
    ///
    /// Release order S08D: сначала click-back `recent_superseded`, потом
    /// старейший primary byproduct. Protected current target не трогаем.
    #[must_use]
    pub fn release_one_for_resource_pressure(
        &mut self,
        protected_key: TimelineHoverPrepareFrameKey,
    ) -> TimelineHoverPreparePressureReleaseOutcome {
        if let Some(released_key) = self.recent_superseded.remove_oldest_for_pressure() {
            return PressureReleaseOutcome::ReleasedRecentSuperseded { released_key };
        }

        if let Some(released_key) = self.remove_oldest_primary_byproduct(protected_key) {
            return PressureReleaseOutcome::ReleasedPrimaryByproduct { released_key };
        }

        let reason = if self.entries.is_empty() {
            PressureReleaseMissReason::NoHoverOwnedEntries
        } else {
            PressureReleaseMissReason::OnlyProtectedCurrentTarget { protected_key }
        };
        PressureReleaseOutcome::NothingReleased { reason }
    }

    /// Вставляет prepared entry и evict-ит самые старые записи сверх capacity.
    ///
    /// Удаление entry просто drop-ает `VideoFrameLease`; release делает сам lease.
    pub fn insert_prepared_frame(
        &mut self,
        key: TimelineHoverPrepareFrameKey,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) {
        self.insert_primary_entry(key, entry);
        self.evict_entries_over_capacity_protecting(key);
    }

    fn insert_primary_entry(
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
    }

    /// Ищет prepared frame: сначала bucket/key, затем actual timing validation.
    #[must_use]
    pub fn lookup_prepared_frame(
        &self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> TimelineHoverPrepareLookupOutcome<'_, BranchToken> {
        match self.find_validated_entry(&request) {
            Ok(entry) => {
                return TimelineHoverPrepareLookupOutcome::Hit(TimelineHoverPreparedFrame {
                    key: request.key,
                    entry,
                });
            }
            Err(primary_failure) => match self.recent_superseded.find_validated_entry(&request) {
                Ok(entry) => TimelineHoverPrepareLookupOutcome::Hit(TimelineHoverPreparedFrame {
                    key: request.key,
                    entry,
                }),
                Err(recent_failure) => {
                    TimelineHoverPrepareLookupFailure::recent_fallback_or_primary(
                        primary_failure,
                        recent_failure,
                    )
                    .into_lookup_outcome()
                }
            },
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
        match self.take_validated_primary_entry(&request) {
            Ok(entry) => {
                return Self::promotion_outcome_from_entry(request.key, entry);
            }
            Err(primary_failure) => match self.recent_superseded.take_validated_entry(&request) {
                Ok(entry) => Self::promotion_outcome_from_entry(request.key, entry),
                Err(recent_failure) => {
                    TimelineHoverPrepareLookupFailure::recent_fallback_or_primary(
                        primary_failure,
                        recent_failure,
                    )
                    .into_promotion_outcome()
                }
            },
        }
    }

    /// Возвращает pre-commit superseded transaction entry в click-back compartment.
    ///
    /// Только `SupersededByNewTarget` имеет право на demote-back. Остальные пути
    /// остаются во владении transaction owner-а и release-ятся там.
    #[must_use]
    pub fn try_demote_promoted_frame_to_recent_superseded(
        &mut self,
        promoted_frame: TimelineHoverPromotedPreparedFrame<BranchToken>,
        request: TimelineHoverPrepareFrameLookupRequest,
        cancel_reason: CancelScrubReason,
    ) -> TimelineHoverPrepareDemoteBackOutcome<BranchToken> {
        if cancel_reason != CancelScrubReason::SupersededByNewTarget {
            return TimelineHoverPrepareDemoteBackOutcome::Rejected {
                promoted_frame,
                reason: TimelineHoverPrepareDemoteBackRejection::CancelReasonDoesNotAllowDemote {
                    actual: cancel_reason,
                },
            };
        }

        if promoted_frame.key != request.key {
            return TimelineHoverPrepareDemoteBackOutcome::Rejected {
                reason: TimelineHoverPrepareDemoteBackRejection::PromotedKeyNotCurrent {
                    promoted_key: promoted_frame.key,
                    current_key: request.key,
                },
                promoted_frame,
            };
        }

        if let Some(rejection) = validate_entry_timing(&request, promoted_frame.entry.timing) {
            return TimelineHoverPrepareDemoteBackOutcome::Rejected {
                promoted_frame,
                reason: TimelineHoverPrepareDemoteBackRejection::TimingRejected(rejection),
            };
        }

        let resource_descriptor = promoted_frame.resource_descriptor();
        if self
            .recent_superseded
            .budget_for_descriptor(resource_descriptor)
            == 0
        {
            return TimelineHoverPrepareDemoteBackOutcome::Rejected {
                reason:
                    TimelineHoverPrepareDemoteBackRejection::RecentSupersededRetentionDisabled {
                        resource_kind: resource_descriptor.kind(),
                    },
                promoted_frame,
            };
        }

        let (key, entry) = promoted_frame.into_validated_entry();
        self.recent_superseded.insert_validated_demoted(key, entry);
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    }

    /// Очищает только `recent_superseded`; primary hover entries не затрагиваются.
    pub fn clear_recent_superseded(
        &mut self,
        _reason: TimelineHoverRecentSupersededClearReason,
    ) -> usize {
        self.recent_superseded.clear()
    }

    fn take_validated_primary_entry(
        &mut self,
        request: &TimelineHoverPrepareFrameLookupRequest,
    ) -> Result<TimelineHoverPreparedFrameEntry<BranchToken>, TimelineHoverPrepareLookupFailure>
    {
        self.find_validated_entry(request)?;

        let entry = self
            .entries
            .remove(&request.key)
            .expect("validated prepared entry must remain present until promotion removes it");
        self.remove_key_from_indexes(request.key);

        Ok(entry)
    }

    fn promotion_outcome_from_entry(
        key: TimelineHoverPrepareFrameKey,
        entry: TimelineHoverPreparedFrameEntry<BranchToken>,
    ) -> TimelineHoverPreparePromotionOutcome<BranchToken> {
        let promoted_frame = TimelineHoverPromotedPreparedFrame::from_validated_entry(key, entry);

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

    fn slot_plan_for_admission(
        &self,
        request: TimelineHoverPrepareAdmissionRequest,
    ) -> Option<TimelineHoverPrepareSlotPlan> {
        if self.entries.contains_key(&request.prepared_key()) {
            return Some(SlotPlan::ReplaceExistingPrimary);
        }

        if self.entries.len() < self.capacity.get() {
            return Some(SlotPlan::UseSparePrimarySlot);
        }

        if request.mode() == AdmissionMode::NormalHover
            && self.has_evictable_primary_byproduct(request.protected_key())
        {
            return Some(SlotPlan::EvictOldestPrimaryByproduct);
        }

        None
    }

    fn has_evictable_primary_byproduct(&self, protected_key: TimelineHoverPrepareFrameKey) -> bool {
        self.insertion_order
            .iter()
            .any(|stored_key| *stored_key != protected_key && self.entries.contains_key(stored_key))
    }

    fn evict_entries_over_capacity_protecting(
        &mut self,
        protected_key: TimelineHoverPrepareFrameKey,
    ) -> usize {
        let mut evicted_entries = 0;
        while self.entries.len() > self.capacity.get() {
            let Some(evicted_key) = self.remove_oldest_primary_byproduct(protected_key) else {
                break;
            };

            debug_assert_ne!(evicted_key, protected_key);
            evicted_entries += 1;
        }
        evicted_entries
    }

    fn remove_oldest_primary_byproduct(
        &mut self,
        protected_key: TimelineHoverPrepareFrameKey,
    ) -> Option<TimelineHoverPrepareFrameKey> {
        let released_key = self.insertion_order.iter().copied().find(|stored_key| {
            *stored_key != protected_key && self.entries.contains_key(stored_key)
        })?;

        self.entries.remove(&released_key);
        self.remove_key_from_indexes(released_key);
        Some(released_key)
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

pub(super) fn classify_key_miss_for_live_bucket_keys(
    requested_key: TimelineHoverPrepareFrameKey,
    live_bucket_keys: impl IntoIterator<Item = TimelineHoverPrepareFrameKey>,
) -> TimelineHoverPrepareLookupMissReason {
    let live_bucket_keys = live_bucket_keys.into_iter().collect::<Vec<_>>();

    if let Some(stored_key) = live_bucket_keys.iter().copied().find(|stored_key| {
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

    if let Some(stored_key) = live_bucket_keys.iter().copied().find(|stored_key| {
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

    if let Some(stored_key) = live_bucket_keys.iter().copied().find(|stored_key| {
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

    if let Some(stored_key) = live_bucket_keys.iter().copied().find(|stored_key| {
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

    if let Some(stored_key) = live_bucket_keys.iter().copied().find(|stored_key| {
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

    TimelineHoverPrepareLookupMissReason::NoEntryForKey
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
