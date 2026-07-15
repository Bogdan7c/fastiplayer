//! Public bounded result-stream vocabulary без domain mutation types.

use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use crate::{
    DiscoveryCancellationCause, ManifestCandidateKey, ProbedLocalMedia, SiblingPolicyRevision,
};

/// Максимум retained verified, но ещё не released records одного job-а.
pub const VERIFIED_RECORD_BUFFER_LIMIT: usize = 512;

/// Максимум records одного exact-once admission batch.
pub const ADMITTED_BATCH_RECORD_LIMIT: usize = 32;

/// Максимум retained typed diagnostics; остальные учитываются счётчиком.
pub const DISCOVERY_DIAGNOSTIC_LIMIT: usize = 64;

/// Opaque process-lifetime ID одного discovery job-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiscoveryJobId(NonZeroU64);

impl DiscoveryJobId {
    pub(crate) fn from_counter(counter: u64) -> Option<Self> {
        NonZeroU64::new(counter).map(Self)
    }

    /// Возвращает opaque numeric value для logs/correlation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// App-owned source/structural revision, захваченная immutable request-ом.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiscoveryRequestRevision(u64);

impl DiscoveryRequestRevision {
    /// Захватывает revision без интерпретации domain semantics.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает opaque correlation value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Request kind поверх одного probe owner-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryJobKind {
    /// Automatic target-centered sibling expansion.
    SiblingDiscovery,
    /// Explicit multi-file Add validation.
    ManualBatch,
    /// Demand-driven visible/current metadata refresh.
    VisibleRefresh,
    /// Transactional metadata-sort preparation.
    MetadataSortPreparation,
}

/// Scheduling class не содержит repeat/shuffle/domain policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiscoveryPriority {
    /// Explicit/navigation work может использовать reserved lane/worker.
    Foreground,
    /// Visibility/bulk work не занимает foreground-only worker.
    Speculative,
}

/// Направление относительно explicit target в natural manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdmissionDirection {
    /// Natural records перед target, от ближайшего наружу.
    Before,
    /// Natural records после target, от ближайшего наружу.
    After,
    /// Non-sibling batch без directional frontier.
    NonDirectional,
}

/// Domain-facing atomicity contract одного request kind-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchApplySemantics {
    /// Sibling batches могут атомарно добавляться по мере D74 admission.
    ProgressiveSiblingCommit,
    /// Все chunks накапливаются и применяются одной транзакцией только после terminal success.
    /// Caller обязан отбросить уже drained accumulation при cancel/failure terminal outcome.
    AccumulateUntilTerminalAtomicApply,
    /// Visible metadata chunk обновляет уже известные app-owned entries.
    MetadataRefreshChunk,
}

/// D43 accounting snapshot в момент exact release-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSideAccounting {
    /// Число уже admitted records этой стороны.
    pub admitted_on_side: usize,
    /// Текущая quota с transfer только от terminal-exhausted другой стороны.
    pub effective_side_quota: usize,
    /// Суммарно admitted sibling records без explicit target.
    pub total_admitted: usize,
}

/// Stable key record-а внутри конкретного job-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryRecordKey {
    /// Key принадлежит immutable directory manifest.
    Manifest(ManifestCandidateKey),
    /// Zero-based key caller-provided batch-а.
    Batch(u32),
}

/// Probe-success payload; Item ID намеренно отсутствует.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryRecord {
    key: DiscoveryRecordKey,
    original_locator: PathBuf,
    media: ProbedLocalMedia,
}

impl DiscoveryRecord {
    pub(crate) fn new(
        key: DiscoveryRecordKey,
        original_locator: PathBuf,
        media: ProbedLocalMedia,
    ) -> Self {
        Self {
            key,
            original_locator,
            media,
        }
    }

    /// Возвращает job-local key.
    #[must_use]
    pub const fn key(&self) -> DiscoveryRecordKey {
        self.key
    }

    /// Возвращает original open/presentation locator без canonical fallback.
    #[must_use]
    pub fn original_locator(&self) -> &Path {
        &self.original_locator
    }

    /// Возвращает immutable probe snapshot.
    #[must_use]
    pub const fn media(&self) -> &ProbedLocalMedia {
        &self.media
    }
}

/// Opaque exact-once release ID; ACK допустим только один раз.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdmissionBatchId(NonZeroU64);

impl AdmissionBatchId {
    pub(crate) fn from_counter(counter: u64) -> Option<Self> {
        NonZeroU64::new(counter).map(Self)
    }

    /// Возвращает opaque correlation value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Единственный public event, который несёт records для будущего atomic commit.
#[derive(Debug)]
pub struct AdmittedBatch {
    job_id: DiscoveryJobId,
    request_revision: DiscoveryRequestRevision,
    policy_revision: Option<SiblingPolicyRevision>,
    batch_id: AdmissionBatchId,
    direction: AdmissionDirection,
    frontier_revision: Option<u64>,
    side_accounting: Option<AdmissionSideAccounting>,
    apply_semantics: BatchApplySemantics,
    records: Box<[DiscoveryRecord]>,
}

pub(crate) struct AdmittedBatchContext {
    pub job_id: DiscoveryJobId,
    pub request_revision: DiscoveryRequestRevision,
    pub policy_revision: Option<SiblingPolicyRevision>,
    pub batch_id: AdmissionBatchId,
    pub direction: AdmissionDirection,
    pub frontier_revision: Option<u64>,
    pub side_accounting: Option<AdmissionSideAccounting>,
    pub apply_semantics: BatchApplySemantics,
}

impl AdmittedBatch {
    pub(crate) fn new(context: AdmittedBatchContext, records: Vec<DiscoveryRecord>) -> Self {
        Self {
            job_id: context.job_id,
            request_revision: context.request_revision,
            policy_revision: context.policy_revision,
            batch_id: context.batch_id,
            direction: context.direction,
            frontier_revision: context.frontier_revision,
            side_accounting: context.side_accounting,
            apply_semantics: context.apply_semantics,
            records: records.into_boxed_slice(),
        }
    }

    /// Возвращает captured source/structural revision.
    #[must_use]
    pub const fn request_revision(&self) -> DiscoveryRequestRevision {
        self.request_revision
    }

    /// Возвращает D62 revision только для sibling job-а.
    #[must_use]
    pub const fn policy_revision(&self) -> Option<SiblingPolicyRevision> {
        self.policy_revision
    }

    /// Коррелирует release с exact job scope.
    #[must_use]
    pub const fn job_id(&self) -> DiscoveryJobId {
        self.job_id
    }

    /// Возвращает token для neutral commit ACK.
    #[must_use]
    pub const fn batch_id(&self) -> AdmissionBatchId {
        self.batch_id
    }

    /// Возвращает side accounting class.
    #[must_use]
    pub const fn direction(&self) -> AdmissionDirection {
        self.direction
    }

    /// Correlates batch with the exact D74 frontier movement for sibling work.
    #[must_use]
    pub const fn frontier_revision(&self) -> Option<u64> {
        self.frontier_revision
    }

    /// Возвращает D43 quota/accounting snapshot только для sibling batch-а.
    #[must_use]
    pub const fn side_accounting(&self) -> Option<AdmissionSideAccounting> {
        self.side_accounting
    }

    /// Объясняет consumer-у, можно ли применять chunk независимо.
    #[must_use]
    pub const fn apply_semantics(&self) -> BatchApplySemantics {
        self.apply_semantics
    }

    /// Records появляются ровно в одном `AdmittedBatch`.
    #[must_use]
    pub fn records(&self) -> &[DiscoveryRecord] {
        &self.records
    }
}

/// Marker-only D74 event: records/Item IDs здесь конструктивно отсутствуют.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionAdvanced {
    job_id: DiscoveryJobId,
    request_revision: DiscoveryRequestRevision,
    policy_revision: Option<SiblingPolicyRevision>,
    direction: AdmissionDirection,
    revision: u64,
    exhausted: bool,
}

impl AdmissionAdvanced {
    pub(crate) const fn new(
        job_id: DiscoveryJobId,
        request_revision: DiscoveryRequestRevision,
        policy_revision: Option<SiblingPolicyRevision>,
        direction: AdmissionDirection,
        revision: u64,
        exhausted: bool,
    ) -> Self {
        Self {
            job_id,
            request_revision,
            policy_revision,
            direction,
            revision,
            exhausted,
        }
    }

    /// Возвращает captured source/structural revision.
    #[must_use]
    pub const fn request_revision(self) -> DiscoveryRequestRevision {
        self.request_revision
    }

    /// Возвращает D62 revision только для sibling job-а.
    #[must_use]
    pub const fn policy_revision(self) -> Option<SiblingPolicyRevision> {
        self.policy_revision
    }

    /// Возвращает owning job.
    #[must_use]
    pub const fn job_id(self) -> DiscoveryJobId {
        self.job_id
    }

    /// Возвращает directional watermark.
    #[must_use]
    pub const fn direction(self) -> AdmissionDirection {
        self.direction
    }

    /// Возвращает monotonic frontier revision.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Сообщает доказанное terminal exhaustion стороны.
    #[must_use]
    pub const fn exhausted(self) -> bool {
        self.exhausted
    }
}

/// Non-shuffle readiness после app ACK exact nearest record-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontierReady {
    job_id: DiscoveryJobId,
    request_revision: DiscoveryRequestRevision,
    policy_revision: Option<SiblingPolicyRevision>,
    direction: AdmissionDirection,
    candidate_key: ManifestCandidateKey,
    revision: u64,
}

impl FrontierReady {
    pub(crate) const fn new(
        job_id: DiscoveryJobId,
        request_revision: DiscoveryRequestRevision,
        policy_revision: Option<SiblingPolicyRevision>,
        direction: AdmissionDirection,
        candidate_key: ManifestCandidateKey,
        revision: u64,
    ) -> Self {
        Self {
            job_id,
            request_revision,
            policy_revision,
            direction,
            candidate_key,
            revision,
        }
    }

    /// Возвращает captured source/structural revision.
    #[must_use]
    pub const fn request_revision(self) -> DiscoveryRequestRevision {
        self.request_revision
    }

    /// Возвращает D62 revision exact sibling scope-а.
    #[must_use]
    pub const fn policy_revision(self) -> Option<SiblingPolicyRevision> {
        self.policy_revision
    }

    /// Возвращает owning job.
    #[must_use]
    pub const fn job_id(self) -> DiscoveryJobId {
        self.job_id
    }

    /// Ready публикуется только для Before/After.
    #[must_use]
    pub const fn direction(self) -> AdmissionDirection {
        self.direction
    }

    /// Exact nearest manifest key; app уже сопоставил ему committed Item ID при ACK.
    #[must_use]
    pub const fn candidate_key(self) -> ManifestCandidateKey {
        self.candidate_key
    }

    /// Возвращает monotonic readiness revision.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }
}

/// Public nonblocking event stream.
#[derive(Debug)]
pub enum DiscoveryEvent {
    /// Record-bearing exact-once release.
    AdmittedBatch(AdmittedBatch),
    /// Marker-only directional movement.
    AdmissionAdvanced(AdmissionAdvanced),
    /// ACK-gated exact nearest non-shuffle readiness.
    FrontierReady(FrontierReady),
}

/// Latest-only progress snapshot; old snapshots могут coalesce-иться.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoveryProgress {
    /// Owning job.
    pub job_id: DiscoveryJobId,
    /// Request kind.
    pub kind: DiscoveryJobKind,
    /// Terminal work units.
    pub processed: usize,
    /// Полное число candidate work units.
    pub total: usize,
}

/// Privacy-safe typed failure category для bounded diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeDiagnosticKind {
    /// Container reader отсутствует.
    UnsupportedContainer,
    /// Container не содержит A/V tracks.
    NoAudioVideoTracks,
    /// Filesystem/source I/O failure.
    IoFailure(io::ErrorKind),
    /// Malformed/other probe failure.
    ProbeFailure,
    /// Manifest source исчез после snapshot.
    MissingAfterSnapshot,
    /// Manifest source identity изменилась.
    SourceChangedAfterSnapshot,
    /// Manifest source недоступен.
    UnavailableAfterSnapshot(io::ErrorKind),
}

/// Один bounded diagnostic без raw parser strings и secret-bearing labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryDiagnostic {
    /// Job-local record key.
    pub key: DiscoveryRecordKey,
    /// Typed safe failure class.
    pub kind: ProbeDiagnosticKind,
}

/// Lossless category counters отдельно от bounded diagnostic details.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryFailureCounts {
    /// Container reader отсутствует.
    pub unsupported_container: usize,
    /// Container не содержит A/V tracks.
    pub no_audio_video_tracks: usize,
    /// I/O, malformed и source-race failures.
    pub probe_failed: usize,
}

impl DiscoveryFailureCounts {
    pub(crate) fn record(&mut self, kind: &ProbeDiagnosticKind) {
        match kind {
            ProbeDiagnosticKind::UnsupportedContainer => {
                self.unsupported_container = self.unsupported_container.saturating_add(1);
            }
            ProbeDiagnosticKind::NoAudioVideoTracks => {
                self.no_audio_video_tracks = self.no_audio_video_tracks.saturating_add(1);
            }
            ProbeDiagnosticKind::IoFailure(_)
            | ProbeDiagnosticKind::ProbeFailure
            | ProbeDiagnosticKind::MissingAfterSnapshot
            | ProbeDiagnosticKind::SourceChangedAfterSnapshot
            | ProbeDiagnosticKind::UnavailableAfterSnapshot(_) => {
                self.probe_failed = self.probe_failed.saturating_add(1);
            }
        }
    }
}

/// Почему job больше не публикует новые batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryFinalOutcome {
    /// Все релевантные work units получили terminal outcome.
    Completed,
    /// D43 cap заполнен deterministic nearest records.
    LimitReached,
    /// Typed cooperative cancellation.
    Cancelled(DiscoveryCancellationCause),
    /// Executor lifecycle завершён.
    ExecutorDisconnected,
}

/// Lossless terminal summary из preallocated job-owned slot-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryFinalSummary {
    /// Owning job.
    pub job_id: DiscoveryJobId,
    /// Request kind.
    pub kind: DiscoveryJobKind,
    /// Captured source/structural revision.
    pub request_revision: DiscoveryRequestRevision,
    /// Immutable D62 revision только для sibling scope.
    pub policy_revision: Option<SiblingPolicyRevision>,
    /// Terminal outcome.
    pub outcome: DiscoveryFinalOutcome,
    /// Успешно verified records, включая ещё ожидающие ACK.
    pub verified: usize,
    /// Probe/source failures.
    pub failed: usize,
    /// Exact typed category counts; details below остаются bounded.
    pub failure_counts: DiscoveryFailureCounts,
    /// Retained bounded diagnostics.
    pub diagnostics: Box<[DiscoveryDiagnostic]>,
    /// Число дополнительных diagnostics, не удержанных в памяти.
    pub omitted_diagnostics: usize,
}

/// Результат neutral app commit ACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionAckOutcome {
    /// Matching batch ACK принят впервые.
    Accepted,
    /// Batch уже ACK-нут, не принадлежал job-у или не требовал readiness ACK.
    /// Manual/sort/visible и later sibling batches не удерживаются в ACK map.
    StaleOrAlreadyAcknowledged,
    /// Admission заморожен; ACK не consumed и может быть повторён после resume.
    AdmissionFrozen,
    /// Job отменён; late ACK не возвращает records к жизни.
    JobTerminated,
}
