use std::time::Duration;

mod hover;
mod prepared;

pub use self::hover::{
    ScrubHoverDependencySpanDiagnosticsCounters, ScrubHoverDependencySpanIncompleteReason,
    ScrubHoverDependencySpanIncompleteReasonCounters, ScrubHoverDependencySpanOutcome,
    ScrubHoverDependencySpanProgress, ScrubHoverNetworkDiagnosticsCounters, ScrubHoverNetworkState,
    ScrubHoverPrepareAdmissionCounters, ScrubHoverPrepareDiagnosticsCounters,
};
pub use self::prepared::{
    ScrubPreparedFrameDemoteRejectionCounters, ScrubPreparedFrameDemoteRejectionKind,
    ScrubPreparedFrameDiagnosticsCounters, ScrubPreparedFrameHitOutcome,
    ScrubPreparedFrameOwnershipCounters, ScrubPreparedFrameOwnershipEvent,
    ScrubPreparedFrameResumePendingReason, ScrubPreparedFrameResumePendingReasonCounters,
    ScrubResumeRunwayState, ScrubResumeRunwayStateCounters,
};

use crate::config::LiveScrubDecodeMode;
use crate::request::{ScrubRequestKind, ScrubStaleReason, ScrubTargetContext};
use crate::scheduler::SchedulerDiagnostic;
use crate::scrub::{
    AudioResumeErrorReason, DecoderBackpressureReason, DemuxUnavailableReason,
    DemuxUnsupportedReason, HostUploadBackpressureReason, ResourceBusyReason, ScrubDriverOutcome,
    ScrubFatalReason, ScrubTargetReachStatus, ScrubTimeoutReason,
};
use crate::working_set::{
    TimelineHoverPrepareAdmissionOutcome, TimelineHoverPrepareInsertOutcome,
    TimelineHoverPrepareLookupOutcome, TimelineHoverPreparePressureReleaseOutcome,
    TimelineHoverPreparePromotionOutcome, TimelineHoverPrepareProviderBudget,
};

/// Driver-only outcome kind, который можно положить в diagnostics без раскрытия payload-а UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubDriverOutcomeKind {
    Prepared,
    DecodePointSeeked,
    Progressed,
    PreTargetReleased,
    ExactFrameReady,
    PreviewFrameReady,
    AudioResumePending,
    AudioResumeTimedOut,
    AudioResumeFailed,
    Finished,
    MatchedPlayback,
    Cancelled,
    StaleGeneration,
    ResourceBusy,
    DemuxUnavailable,
    DemuxUnsupported,
    DecoderBackpressure,
    HostUploadBackpressure,
    TimedOut,
    Fatal,
}

/// Public phase для event consumers, без driver lifecycle деталей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPublicPhase {
    Started,
    Progress,
    PreviewFrameReady,
    ResumePending,
    Committed,
    MatchedPlayback,
    Cancelled,
    Failed,
}

/// Нормализованная public failure category. Driver payload остаётся в outcome/diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubFailureReason {
    AudioResumeTimedOut,
    AudioResumeFailed,
    DemuxUnavailable,
    DemuxUnsupported,
    DecoderBackpressure,
    HostUploadBackpressure,
    ResourceBusy,
    Timeout,
    Fatal,
}

/// Typed driver detail для diagnostics. Public event reason остаётся normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubDriverDiagnosticReason {
    AudioResumeError(AudioResumeErrorReason),
    DemuxUnavailable(DemuxUnavailableReason),
    DemuxUnsupported(DemuxUnsupportedReason),
    DecoderBackpressure(DecoderBackpressureReason),
    HostUploadBackpressure(HostUploadBackpressureReason),
    ResourceBusy(ResourceBusyReason),
    Timeout(ScrubTimeoutReason),
    Fatal(ScrubFatalReason),
    StaleGeneration(ScrubStaleReason),
}

/// Snapshot live-scrub settings, захваченный owner-ом gesture-а на pointer-down.
///
/// Тип нейтрален к UI: он описывает только decode policy, которую player должен
/// видеть в diagnostics для конкретного drag, но не управляет реальным lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiveScrubSettingsSnapshot {
    /// Policy запуска decode work для этого drag gesture-а.
    pub decode_mode: LiveScrubDecodeMode,

    /// Валидированный верхний лимит decode attempts для `ThrottledLatest`.
    pub max_hz: u16,
}

/// Последнее изменение live-scrub settings, отложенное до следующего drag-а.
///
/// Здесь нет timestamp/drag id/target: это bounded diagnostics, а не event history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeferredLiveScrubSettingsChange {
    /// Snapshot, от которого пользовательские настройки ушли во время active drag.
    pub old_snapshot: LiveScrubSettingsSnapshot,

    /// Новый committed snapshot, который будет использован только следующим drag-ом.
    pub new_snapshot: LiveScrubSettingsSnapshot,
}

/// Bounded diagnostics одного live-scrub gesture-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiveScrubDiagnostics {
    /// Snapshot, реально управляющий текущим gesture-ом до release/cancel.
    pub settings_snapshot: LiveScrubSettingsSnapshot,

    /// Сколько distinct settings changes было отложено для текущего gesture-а.
    pub deferred_live_scrub_settings_change_count: u64,

    /// Только последнее отложенное изменение; history/ring buffer намеренно нет.
    pub latest_deferred_live_scrub_settings_change: Option<DeferredLiveScrubSettingsChange>,

    /// Сколько intermediate targets UI-side `ThrottledLatest` не отправил в decoder work.
    pub throttled_latest_skip_count: u64,
}

impl LiveScrubDiagnostics {
    /// Создаёт diagnostics для нового live-scrub gesture-а без deferred changes.
    #[must_use]
    pub const fn from_settings_snapshot(settings_snapshot: LiveScrubSettingsSnapshot) -> Self {
        Self {
            settings_snapshot,
            deferred_live_scrub_settings_change_count: 0,
            latest_deferred_live_scrub_settings_change: None,
            throttled_latest_skip_count: 0,
        }
    }

    /// Запоминает latest-only deferred settings change без накопления истории.
    pub fn record_deferred_settings_change(&mut self, change: DeferredLiveScrubSettingsChange) {
        self.deferred_live_scrub_settings_change_count = self
            .deferred_live_scrub_settings_change_count
            .saturating_add(1);
        self.latest_deferred_live_scrub_settings_change = Some(change);
    }

    /// Записывает UI-side throttle skip отдельно от decoder/resource pressure.
    pub fn record_throttled_latest_skip(&mut self) {
        self.throttled_latest_skip_count = self.throttled_latest_skip_count.saturating_add(1);
    }
}

/// Diagnostics, которые связывают public event с исходным driver outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubEventDiagnostics {
    pub driver_outcome: ScrubDriverOutcomeKind,
    pub driver_reason: Option<ScrubDriverDiagnosticReason>,
    pub stale_reason: Option<ScrubStaleReason>,
    pub live_scrub: Option<LiveScrubDiagnostics>,
}

impl ScrubEventDiagnostics {
    #[must_use]
    pub const fn new(driver_outcome: ScrubDriverOutcomeKind) -> Self {
        Self {
            driver_outcome,
            driver_reason: None,
            stale_reason: None,
            live_scrub: None,
        }
    }

    #[must_use]
    pub const fn with_driver_reason(
        driver_outcome: ScrubDriverOutcomeKind,
        driver_reason: ScrubDriverDiagnosticReason,
    ) -> Self {
        Self {
            driver_outcome,
            driver_reason: Some(driver_reason),
            stale_reason: None,
            live_scrub: None,
        }
    }

    #[must_use]
    pub const fn with_stale_reason(
        driver_outcome: ScrubDriverOutcomeKind,
        stale_reason: ScrubStaleReason,
    ) -> Self {
        Self {
            driver_outcome,
            driver_reason: Some(ScrubDriverDiagnosticReason::StaleGeneration(stale_reason)),
            stale_reason: Some(stale_reason),
            live_scrub: None,
        }
    }

    /// Прикрепляет bounded live-scrub diagnostics, не меняя public event phase.
    #[must_use]
    pub const fn with_live_scrub(mut self, live_scrub: LiveScrubDiagnostics) -> Self {
        self.live_scrub = Some(live_scrub);
        self
    }
}

/// Общая часть event payload-а для future diagnostics snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubEventEnvelope {
    pub context: ScrubTargetContext,
    pub phase: ScrubPublicPhase,
    pub diagnostics: ScrubEventDiagnostics,
}

/// Snapshot накопленных diagnostics без истории событий.
///
/// Все поля copyable и bounded: такой снимок можно дешёво отдать UI/тестам,
/// не раскрывая внутренние очереди scheduler-а, decoder-а или working set-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubDiagnosticsSnapshot {
    pub requests: ScrubRequestLifecycleCounters,
    pub outcomes: ScrubDriverOutcomeCounters,
    pub driver_reasons: ScrubDriverDiagnosticReasonCounters,
    pub scheduler: ScrubSchedulerDiagnosticCounters,
    pub resource_pressure: ScrubResourcePressureCounters,
    pub queue_age: DurationSummary,
    pub decode_latency: DurationSummary,
    pub demux_seek_latency: DurationSummary,
    pub packets_from_decode_point_to_target: CountSummary,
    pub pre_target_frame_drops: CountSummary,
    pub prepared_frames: ScrubPreparedFrameDiagnosticsCounters,
    pub working_set: ScrubWorkingSetDiagnosticsCounters,
    pub hover_prepare: ScrubHoverPrepareDiagnosticsCounters,
    pub latest_live_scrub: Option<LiveScrubDiagnostics>,
}

/// Mutable accumulator для scrub diagnostics.
///
/// Recorder принимает только typed boundary results и простые наблюдения
/// длительности/счётчиков. Он не владеет runtime state и не хранит payload-ы
/// событий, поэтому его можно безопасно подключать позже в player-owned flow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubDiagnosticsRecorder {
    snapshot: ScrubDiagnosticsSnapshot,
}

impl ScrubDiagnosticsRecorder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            snapshot: ScrubDiagnosticsSnapshot::new(),
        }
    }

    /// Возвращает bounded copy текущих метрик без сброса accumulator-а.
    #[must_use]
    pub const fn snapshot(&self) -> ScrubDiagnosticsSnapshot {
        self.snapshot
    }

    pub fn record_request_accepted(&mut self, request_kind: ScrubRequestKind) {
        self.snapshot.requests.accepted.increment(request_kind);
    }

    pub fn record_request_cancelled(&mut self, request_kind: ScrubRequestKind) {
        self.snapshot.requests.cancelled.increment(request_kind);
    }

    pub fn record_request_completed(&mut self, request_kind: ScrubRequestKind) {
        self.snapshot.requests.completed.increment(request_kind);
    }

    pub fn record_queue_age(&mut self, queue_age: Duration) {
        self.snapshot.queue_age.record(queue_age);
    }

    pub fn record_decode_latency(&mut self, decode_latency: Duration) {
        self.snapshot.decode_latency.record(decode_latency);
    }

    pub fn record_demux_seek_latency(&mut self, demux_seek_latency: Duration) {
        self.snapshot.demux_seek_latency.record(demux_seek_latency);
    }

    pub fn record_packets_from_decode_point_to_target(&mut self, packets: u64) {
        self.snapshot
            .packets_from_decode_point_to_target
            .record(packets);
    }

    pub fn record_pre_target_frame_drops(&mut self, dropped_frames: u64) {
        self.snapshot.pre_target_frame_drops.record(dropped_frames);
    }

    pub fn record_prepared_frame_hit(&mut self, outcome: ScrubPreparedFrameHitOutcome) {
        self.snapshot.prepared_frames.record_prepared_hit(outcome);
    }

    pub fn record_cold_exact_decode_pending(&mut self) {
        self.snapshot
            .prepared_frames
            .record_cold_exact_decode_pending();
    }

    pub fn record_prepared_frame_ownership_event(
        &mut self,
        event: ScrubPreparedFrameOwnershipEvent,
    ) {
        self.snapshot.prepared_frames.record_ownership_event(event);
    }

    /// Записывает typed driver outcome без нормализации в public UI phase.
    pub fn record_driver_outcome(&mut self, outcome: &ScrubDriverOutcome) {
        self.snapshot.outcomes.increment(outcome.kind());

        match outcome {
            ScrubDriverOutcome::Progressed(payload)
                if payload.progress.target_status == ScrubTargetReachStatus::BeforeTarget =>
            {
                self.snapshot.outcomes.cold_decode_in_progress = self
                    .snapshot
                    .outcomes
                    .cold_decode_in_progress
                    .saturating_add(1);
            }
            ScrubDriverOutcome::AudioResumeFailed(payload) => self.record_driver_reason(
                ScrubDriverDiagnosticReason::AudioResumeError(payload.reason),
            ),
            ScrubDriverOutcome::StaleGeneration(payload) => self
                .record_driver_reason(ScrubDriverDiagnosticReason::StaleGeneration(payload.reason)),
            ScrubDriverOutcome::ResourceBusy(payload) => {
                self.record_driver_reason(ScrubDriverDiagnosticReason::ResourceBusy(payload.reason))
            }
            ScrubDriverOutcome::DemuxUnavailable(payload) => self.record_driver_reason(
                ScrubDriverDiagnosticReason::DemuxUnavailable(payload.reason),
            ),
            ScrubDriverOutcome::DemuxUnsupported(payload) => self.record_driver_reason(
                ScrubDriverDiagnosticReason::DemuxUnsupported(payload.reason),
            ),
            ScrubDriverOutcome::DecoderBackpressure(payload) => self.record_driver_reason(
                ScrubDriverDiagnosticReason::DecoderBackpressure(payload.reason),
            ),
            ScrubDriverOutcome::HostUploadBackpressure(payload) => self.record_driver_reason(
                ScrubDriverDiagnosticReason::HostUploadBackpressure(payload.reason),
            ),
            ScrubDriverOutcome::TimedOut(payload) => {
                self.record_driver_reason(ScrubDriverDiagnosticReason::Timeout(payload.reason));
            }
            ScrubDriverOutcome::Fatal(payload) => {
                self.record_driver_reason(ScrubDriverDiagnosticReason::Fatal(payload.reason));
            }
            _ => {}
        }
    }

    /// Записывает diagnostics из normalized event, если caller уже потерял outcome payload.
    pub fn record_event_diagnostics(&mut self, diagnostics: ScrubEventDiagnostics) {
        self.snapshot.outcomes.increment(diagnostics.driver_outcome);
        if let Some(live_scrub) = diagnostics.live_scrub {
            self.snapshot.latest_live_scrub = Some(live_scrub);
        }
        if let Some(reason) = diagnostics.driver_reason {
            self.record_driver_reason(reason);
        } else if let Some(stale_reason) = diagnostics.stale_reason {
            self.record_driver_reason(ScrubDriverDiagnosticReason::StaleGeneration(stale_reason));
        }
    }

    /// Записывает scheduler-only signal без скрытого изменения request lifecycle counters.
    pub fn record_scheduler_diagnostic(&mut self, diagnostic: &SchedulerDiagnostic) {
        self.snapshot.scheduler.increment(diagnostic);
    }

    pub fn record_working_set_lookup_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPrepareLookupOutcome<'_, BranchToken>,
    ) {
        self.snapshot.working_set.record_lookup_outcome(outcome);
    }

    pub fn record_working_set_promotion_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPreparePromotionOutcome<BranchToken>,
    ) {
        self.snapshot.working_set.record_promotion_outcome(outcome);
    }

    pub fn record_working_set_insert_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPrepareInsertOutcome<BranchToken>,
    ) {
        self.snapshot.working_set.record_insert_outcome(outcome);
    }

    pub fn record_working_set_pressure_release_outcome(
        &mut self,
        outcome: TimelineHoverPreparePressureReleaseOutcome,
    ) {
        self.snapshot
            .working_set
            .record_pressure_release_outcome(outcome);
    }

    pub fn record_working_set_hit(&mut self) {
        self.snapshot.working_set.record_hit();
    }

    pub fn record_working_set_miss(&mut self) {
        self.snapshot.working_set.record_miss();
    }

    pub fn record_working_set_evictions(&mut self, evicted_entries: u64) {
        self.snapshot.working_set.record_evictions(evicted_entries);
    }

    pub fn record_hover_prepare_admission_outcome(
        &mut self,
        outcome: &TimelineHoverPrepareAdmissionOutcome,
    ) {
        self.snapshot
            .hover_prepare
            .admission
            .record_outcome(outcome);
    }

    pub fn record_hover_prepare_provider_budget(
        &mut self,
        provider_budget: TimelineHoverPrepareProviderBudget,
    ) {
        self.snapshot
            .hover_prepare
            .admission
            .record_provider_budget(provider_budget);
    }

    pub fn record_hover_dependency_span_outcome(
        &mut self,
        outcome: ScrubHoverDependencySpanOutcome,
    ) {
        self.snapshot
            .hover_prepare
            .dependency_span
            .record_outcome(outcome);
    }

    pub fn record_hover_dependency_span_progress(
        &mut self,
        progress: ScrubHoverDependencySpanProgress,
    ) {
        self.snapshot
            .hover_prepare
            .dependency_span
            .record_progress(progress);
    }

    pub fn record_hover_network_state(&mut self, state: ScrubHoverNetworkState) {
        self.snapshot.hover_prepare.network.record_state(state);
    }

    pub fn record_hover_network_zero_throttle_no_delay(&mut self) {
        self.snapshot
            .hover_prepare
            .network
            .record_zero_throttle_no_delay();
    }

    pub fn record_hover_network_latest_only_replaced_in_flight(&mut self) {
        self.snapshot
            .hover_prepare
            .network
            .record_latest_only_replaced_in_flight();
    }

    pub fn record_hover_network_stale_late_result_ignored(&mut self) {
        self.snapshot
            .hover_prepare
            .network
            .record_stale_late_result_ignored();
    }

    pub fn record_hover_network_throttle_delay(&mut self, delay: Duration) {
        self.snapshot
            .hover_prepare
            .network
            .record_throttle_delay(delay);
    }

    pub fn record_driver_reason(&mut self, reason: ScrubDriverDiagnosticReason) {
        self.snapshot.driver_reasons.increment(reason);
        self.snapshot.resource_pressure.record_driver_reason(reason);
    }
}

impl ScrubDiagnosticsSnapshot {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            requests: ScrubRequestLifecycleCounters::new(),
            outcomes: ScrubDriverOutcomeCounters::new(),
            driver_reasons: ScrubDriverDiagnosticReasonCounters::new(),
            scheduler: ScrubSchedulerDiagnosticCounters::new(),
            resource_pressure: ScrubResourcePressureCounters::new(),
            queue_age: DurationSummary::new(),
            decode_latency: DurationSummary::new(),
            demux_seek_latency: DurationSummary::new(),
            packets_from_decode_point_to_target: CountSummary::new(),
            pre_target_frame_drops: CountSummary::new(),
            prepared_frames: ScrubPreparedFrameDiagnosticsCounters::new(),
            working_set: ScrubWorkingSetDiagnosticsCounters::new(),
            hover_prepare: ScrubHoverPrepareDiagnosticsCounters::new(),
            latest_live_scrub: None,
        }
    }
}

/// Сводка длительностей: сколько наблюдений было, сумма, минимум и максимум.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DurationSummary {
    pub samples: u64,
    pub total: Duration,
    pub min: Option<Duration>,
    pub max: Option<Duration>,
}

impl DurationSummary {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: 0,
            total: Duration::ZERO,
            min: None,
            max: None,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.samples == 0
    }

    pub fn record(&mut self, duration: Duration) {
        self.samples = self.samples.saturating_add(1);
        self.total = self.total.saturating_add(duration);
        self.min = Some(match self.min {
            Some(current_min) => current_min.min(duration),
            None => duration,
        });
        self.max = Some(match self.max {
            Some(current_max) => current_max.max(duration),
            None => duration,
        });
    }
}

/// Сводка числовых наблюдений: полезна для packet counts и frame-drop counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CountSummary {
    pub samples: u64,
    pub total: u64,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

impl CountSummary {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: 0,
            total: 0,
            min: None,
            max: None,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.samples == 0
    }

    pub fn record(&mut self, count: u64) {
        self.samples = self.samples.saturating_add(1);
        self.total = self.total.saturating_add(count);
        self.min = Some(match self.min {
            Some(current_min) => current_min.min(count),
            None => count,
        });
        self.max = Some(match self.max {
            Some(current_max) => current_max.max(count),
            None => count,
        });
    }
}

/// Lifecycle counters отдельно по каждому источнику scrub request-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubRequestLifecycleCounters {
    pub accepted: ScrubRequestKindCounters,
    pub cancelled: ScrubRequestKindCounters,
    pub completed: ScrubRequestKindCounters,
}

impl ScrubRequestLifecycleCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            accepted: ScrubRequestKindCounters::new(),
            cancelled: ScrubRequestKindCounters::new(),
            completed: ScrubRequestKindCounters::new(),
        }
    }
}

/// Typed counters by `ScrubRequestKind`, без string keys и без HashMap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubRequestKindCounters {
    pub seek_landing: u64,
    pub live_scrub: u64,
    pub hover_preview: u64,
    pub timeline_hover_prepare_window: u64,
}

impl ScrubRequestKindCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seek_landing: 0,
            live_scrub: 0,
            hover_preview: 0,
            timeline_hover_prepare_window: 0,
        }
    }

    #[must_use]
    pub const fn get(self, request_kind: ScrubRequestKind) -> u64 {
        match request_kind {
            ScrubRequestKind::SeekLanding => self.seek_landing,
            ScrubRequestKind::LiveScrub => self.live_scrub,
            ScrubRequestKind::HoverPreview => self.hover_preview,
            ScrubRequestKind::TimelineHoverPrepareWindow => self.timeline_hover_prepare_window,
        }
    }

    pub fn increment(&mut self, request_kind: ScrubRequestKind) {
        match request_kind {
            ScrubRequestKind::SeekLanding => {
                self.seek_landing = self.seek_landing.saturating_add(1);
            }
            ScrubRequestKind::LiveScrub => {
                self.live_scrub = self.live_scrub.saturating_add(1);
            }
            ScrubRequestKind::HoverPreview => {
                self.hover_preview = self.hover_preview.saturating_add(1);
            }
            ScrubRequestKind::TimelineHoverPrepareWindow => {
                self.timeline_hover_prepare_window =
                    self.timeline_hover_prepare_window.saturating_add(1);
            }
        }
    }
}

/// Driver outcome counters сохраняют все S06/S10 distinctions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubDriverOutcomeCounters {
    pub prepared: u64,
    pub decode_point_seeked: u64,
    pub progressed: u64,
    /// `Progressed` before target means cold decode is still feeding/draining toward target.
    pub cold_decode_in_progress: u64,
    pub pre_target_released: u64,
    pub exact_frame_ready: u64,
    pub preview_frame_ready: u64,
    pub audio_resume_pending: u64,
    pub audio_resume_timed_out: u64,
    pub audio_resume_failed: u64,
    pub finished: u64,
    pub matched_playback: u64,
    pub cancelled: u64,
    pub stale_generation: u64,
    pub resource_busy: u64,
    pub demux_unavailable: u64,
    pub demux_unsupported: u64,
    pub decoder_backpressure: u64,
    pub host_upload_backpressure: u64,
    pub timed_out: u64,
    pub fatal: u64,
}

impl ScrubDriverOutcomeCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prepared: 0,
            decode_point_seeked: 0,
            progressed: 0,
            cold_decode_in_progress: 0,
            pre_target_released: 0,
            exact_frame_ready: 0,
            preview_frame_ready: 0,
            audio_resume_pending: 0,
            audio_resume_timed_out: 0,
            audio_resume_failed: 0,
            finished: 0,
            matched_playback: 0,
            cancelled: 0,
            stale_generation: 0,
            resource_busy: 0,
            demux_unavailable: 0,
            demux_unsupported: 0,
            decoder_backpressure: 0,
            host_upload_backpressure: 0,
            timed_out: 0,
            fatal: 0,
        }
    }

    #[must_use]
    pub const fn get(self, outcome_kind: ScrubDriverOutcomeKind) -> u64 {
        match outcome_kind {
            ScrubDriverOutcomeKind::Prepared => self.prepared,
            ScrubDriverOutcomeKind::DecodePointSeeked => self.decode_point_seeked,
            ScrubDriverOutcomeKind::Progressed => self.progressed,
            ScrubDriverOutcomeKind::PreTargetReleased => self.pre_target_released,
            ScrubDriverOutcomeKind::ExactFrameReady => self.exact_frame_ready,
            ScrubDriverOutcomeKind::PreviewFrameReady => self.preview_frame_ready,
            ScrubDriverOutcomeKind::AudioResumePending => self.audio_resume_pending,
            ScrubDriverOutcomeKind::AudioResumeTimedOut => self.audio_resume_timed_out,
            ScrubDriverOutcomeKind::AudioResumeFailed => self.audio_resume_failed,
            ScrubDriverOutcomeKind::Finished => self.finished,
            ScrubDriverOutcomeKind::MatchedPlayback => self.matched_playback,
            ScrubDriverOutcomeKind::Cancelled => self.cancelled,
            ScrubDriverOutcomeKind::StaleGeneration => self.stale_generation,
            ScrubDriverOutcomeKind::ResourceBusy => self.resource_busy,
            ScrubDriverOutcomeKind::DemuxUnavailable => self.demux_unavailable,
            ScrubDriverOutcomeKind::DemuxUnsupported => self.demux_unsupported,
            ScrubDriverOutcomeKind::DecoderBackpressure => self.decoder_backpressure,
            ScrubDriverOutcomeKind::HostUploadBackpressure => self.host_upload_backpressure,
            ScrubDriverOutcomeKind::TimedOut => self.timed_out,
            ScrubDriverOutcomeKind::Fatal => self.fatal,
        }
    }

    pub fn increment(&mut self, outcome_kind: ScrubDriverOutcomeKind) {
        match outcome_kind {
            ScrubDriverOutcomeKind::Prepared => {
                self.prepared = self.prepared.saturating_add(1);
            }
            ScrubDriverOutcomeKind::DecodePointSeeked => {
                self.decode_point_seeked = self.decode_point_seeked.saturating_add(1);
            }
            ScrubDriverOutcomeKind::Progressed => {
                self.progressed = self.progressed.saturating_add(1);
            }
            ScrubDriverOutcomeKind::PreTargetReleased => {
                self.pre_target_released = self.pre_target_released.saturating_add(1);
            }
            ScrubDriverOutcomeKind::ExactFrameReady => {
                self.exact_frame_ready = self.exact_frame_ready.saturating_add(1);
            }
            ScrubDriverOutcomeKind::PreviewFrameReady => {
                self.preview_frame_ready = self.preview_frame_ready.saturating_add(1);
            }
            ScrubDriverOutcomeKind::AudioResumePending => {
                self.audio_resume_pending = self.audio_resume_pending.saturating_add(1);
            }
            ScrubDriverOutcomeKind::AudioResumeTimedOut => {
                self.audio_resume_timed_out = self.audio_resume_timed_out.saturating_add(1);
            }
            ScrubDriverOutcomeKind::AudioResumeFailed => {
                self.audio_resume_failed = self.audio_resume_failed.saturating_add(1);
            }
            ScrubDriverOutcomeKind::Finished => {
                self.finished = self.finished.saturating_add(1);
            }
            ScrubDriverOutcomeKind::MatchedPlayback => {
                self.matched_playback = self.matched_playback.saturating_add(1);
            }
            ScrubDriverOutcomeKind::Cancelled => {
                self.cancelled = self.cancelled.saturating_add(1);
            }
            ScrubDriverOutcomeKind::StaleGeneration => {
                self.stale_generation = self.stale_generation.saturating_add(1);
            }
            ScrubDriverOutcomeKind::ResourceBusy => {
                self.resource_busy = self.resource_busy.saturating_add(1);
            }
            ScrubDriverOutcomeKind::DemuxUnavailable => {
                self.demux_unavailable = self.demux_unavailable.saturating_add(1);
            }
            ScrubDriverOutcomeKind::DemuxUnsupported => {
                self.demux_unsupported = self.demux_unsupported.saturating_add(1);
            }
            ScrubDriverOutcomeKind::DecoderBackpressure => {
                self.decoder_backpressure = self.decoder_backpressure.saturating_add(1);
            }
            ScrubDriverOutcomeKind::HostUploadBackpressure => {
                self.host_upload_backpressure = self.host_upload_backpressure.saturating_add(1);
            }
            ScrubDriverOutcomeKind::TimedOut => {
                self.timed_out = self.timed_out.saturating_add(1);
            }
            ScrubDriverOutcomeKind::Fatal => {
                self.fatal = self.fatal.saturating_add(1);
            }
        }
    }
}

/// Counters для typed driver diagnostic reasons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubDriverDiagnosticReasonCounters {
    pub audio_resume_error: u64,
    pub demux_unavailable: u64,
    pub demux_unsupported: u64,
    pub decoder_backpressure: u64,
    pub host_upload_backpressure: u64,
    pub resource_busy: u64,
    pub timeout: u64,
    pub fatal: u64,
    pub stale_generation: u64,
}

impl ScrubDriverDiagnosticReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            audio_resume_error: 0,
            demux_unavailable: 0,
            demux_unsupported: 0,
            decoder_backpressure: 0,
            host_upload_backpressure: 0,
            resource_busy: 0,
            timeout: 0,
            fatal: 0,
            stale_generation: 0,
        }
    }

    pub fn increment(&mut self, reason: ScrubDriverDiagnosticReason) {
        match reason {
            ScrubDriverDiagnosticReason::AudioResumeError(_) => {
                self.audio_resume_error = self.audio_resume_error.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::DemuxUnavailable(_) => {
                self.demux_unavailable = self.demux_unavailable.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::DemuxUnsupported(_) => {
                self.demux_unsupported = self.demux_unsupported.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::DecoderBackpressure(_) => {
                self.decoder_backpressure = self.decoder_backpressure.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::HostUploadBackpressure(_) => {
                self.host_upload_backpressure = self.host_upload_backpressure.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::ResourceBusy(_) => {
                self.resource_busy = self.resource_busy.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::Timeout(_) => {
                self.timeout = self.timeout.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::Fatal(_) => {
                self.fatal = self.fatal.saturating_add(1);
            }
            ScrubDriverDiagnosticReason::StaleGeneration(_) => {
                self.stale_generation = self.stale_generation.saturating_add(1);
            }
        }
    }
}

/// Scheduler diagnostics отдельно от request lifecycle, чтобы не было double-count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubSchedulerDiagnosticCounters {
    pub live_scrub_throttled: u64,
    pub active_work_cancelled: u64,
    pub hover_prepare_window_budget_exceeded: u64,
    pub active_completion_ignored: u64,
}

impl ScrubSchedulerDiagnosticCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            live_scrub_throttled: 0,
            active_work_cancelled: 0,
            hover_prepare_window_budget_exceeded: 0,
            active_completion_ignored: 0,
        }
    }

    pub fn increment(&mut self, diagnostic: &SchedulerDiagnostic) {
        match diagnostic {
            SchedulerDiagnostic::LiveScrubThrottled { .. } => {
                self.live_scrub_throttled = self.live_scrub_throttled.saturating_add(1);
            }
            SchedulerDiagnostic::ActiveWorkCancelled { .. } => {
                self.active_work_cancelled = self.active_work_cancelled.saturating_add(1);
            }
            SchedulerDiagnostic::HoverPrepareWindowBudgetExceeded { .. } => {
                self.hover_prepare_window_budget_exceeded =
                    self.hover_prepare_window_budget_exceeded.saturating_add(1);
            }
            SchedulerDiagnostic::ActiveCompletionIgnored { .. } => {
                self.active_completion_ignored = self.active_completion_ignored.saturating_add(1);
            }
        }
    }
}

/// Resource pressure counters не смешивают decoder и host-upload backpressure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubResourcePressureCounters {
    pub resource_busy: u64,
    pub decoder_backpressure: u64,
    pub host_upload_backpressure: u64,
    pub resource_busy_reasons: ResourceBusyReasonCounters,
    pub decoder_backpressure_reasons: DecoderBackpressureReasonCounters,
    pub host_upload_backpressure_reasons: HostUploadBackpressureReasonCounters,
}

impl ScrubResourcePressureCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            resource_busy: 0,
            decoder_backpressure: 0,
            host_upload_backpressure: 0,
            resource_busy_reasons: ResourceBusyReasonCounters::new(),
            decoder_backpressure_reasons: DecoderBackpressureReasonCounters::new(),
            host_upload_backpressure_reasons: HostUploadBackpressureReasonCounters::new(),
        }
    }

    pub fn record_driver_reason(&mut self, reason: ScrubDriverDiagnosticReason) {
        match reason {
            ScrubDriverDiagnosticReason::ResourceBusy(resource_busy_reason) => {
                self.resource_busy = self.resource_busy.saturating_add(1);
                self.resource_busy_reasons.increment(resource_busy_reason);
            }
            ScrubDriverDiagnosticReason::DecoderBackpressure(backpressure_reason) => {
                self.decoder_backpressure = self.decoder_backpressure.saturating_add(1);
                self.decoder_backpressure_reasons
                    .increment(backpressure_reason);
            }
            ScrubDriverDiagnosticReason::HostUploadBackpressure(backpressure_reason) => {
                self.host_upload_backpressure = self.host_upload_backpressure.saturating_add(1);
                self.host_upload_backpressure_reasons
                    .increment(backpressure_reason);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ResourceBusyReasonCounters {
    pub playback_owns_decoder: u64,
    pub preview_lease_still_held: u64,
    pub backend_resource_pressure: u64,
}

impl ResourceBusyReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            playback_owns_decoder: 0,
            preview_lease_still_held: 0,
            backend_resource_pressure: 0,
        }
    }

    pub fn increment(&mut self, reason: ResourceBusyReason) {
        match reason {
            ResourceBusyReason::PlaybackOwnsDecoder => {
                self.playback_owns_decoder = self.playback_owns_decoder.saturating_add(1);
            }
            ResourceBusyReason::PreviewLeaseStillHeld => {
                self.preview_lease_still_held = self.preview_lease_still_held.saturating_add(1);
            }
            ResourceBusyReason::BackendResourcePressure => {
                self.backend_resource_pressure = self.backend_resource_pressure.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DecoderBackpressureReasonCounters {
    pub packet_queue_full: u64,
    pub output_floor_control_blocked: u64,
    pub decoder_control_channel_full: u64,
}

impl DecoderBackpressureReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            packet_queue_full: 0,
            output_floor_control_blocked: 0,
            decoder_control_channel_full: 0,
        }
    }

    pub fn increment(&mut self, reason: DecoderBackpressureReason) {
        match reason {
            DecoderBackpressureReason::PacketQueueFull => {
                self.packet_queue_full = self.packet_queue_full.saturating_add(1);
            }
            DecoderBackpressureReason::OutputFloorControlBlocked => {
                self.output_floor_control_blocked =
                    self.output_floor_control_blocked.saturating_add(1);
            }
            DecoderBackpressureReason::DecoderControlChannelFull => {
                self.decoder_control_channel_full =
                    self.decoder_control_channel_full.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct HostUploadBackpressureReasonCounters {
    pub ready_frame_queue_full: u64,
    pub upload_slots_exhausted: u64,
    pub upload_control_channel_full: u64,
}

impl HostUploadBackpressureReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ready_frame_queue_full: 0,
            upload_slots_exhausted: 0,
            upload_control_channel_full: 0,
        }
    }

    pub fn increment(&mut self, reason: HostUploadBackpressureReason) {
        match reason {
            HostUploadBackpressureReason::ReadyFrameQueueFull => {
                self.ready_frame_queue_full = self.ready_frame_queue_full.saturating_add(1);
            }
            HostUploadBackpressureReason::UploadSlotsExhausted => {
                self.upload_slots_exhausted = self.upload_slots_exhausted.saturating_add(1);
            }
            HostUploadBackpressureReason::UploadControlChannelFull => {
                self.upload_control_channel_full =
                    self.upload_control_channel_full.saturating_add(1);
            }
        }
    }
}

/// Working-set counters: hit/miss/eviction без хранения frame lease/debug payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubWorkingSetDiagnosticsCounters {
    pub hits: u64,
    pub misses: u64,
    pub timing_rejections: u64,
    pub evictions: u64,
    pub promotion_hits: u64,
    pub promotion_misses: u64,
    pub pressure_release_misses: u64,
    pub released_recent_superseded: u64,
    pub released_primary_byproduct: u64,
}

impl ScrubWorkingSetDiagnosticsCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hits: 0,
            misses: 0,
            timing_rejections: 0,
            evictions: 0,
            promotion_hits: 0,
            promotion_misses: 0,
            pressure_release_misses: 0,
            released_recent_superseded: 0,
            released_primary_byproduct: 0,
        }
    }

    pub fn record_lookup_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPrepareLookupOutcome<'_, BranchToken>,
    ) {
        match outcome {
            TimelineHoverPrepareLookupOutcome::Hit(_) => self.record_hit(),
            TimelineHoverPrepareLookupOutcome::Miss(_) => self.record_miss(),
            TimelineHoverPrepareLookupOutcome::TimingRejected(_) => {
                self.timing_rejections = self.timing_rejections.saturating_add(1);
            }
        }
    }

    pub fn record_promotion_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPreparePromotionOutcome<BranchToken>,
    ) {
        match outcome {
            TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(_)
            | TimelineHoverPreparePromotionOutcome::PromotedVisualOverrideResumePending(_) => {
                self.record_hit();
                self.promotion_hits = self.promotion_hits.saturating_add(1);
            }
            TimelineHoverPreparePromotionOutcome::Miss(_) => {
                self.record_miss();
                self.promotion_misses = self.promotion_misses.saturating_add(1);
            }
            TimelineHoverPreparePromotionOutcome::TimingRejected(_) => {
                self.timing_rejections = self.timing_rejections.saturating_add(1);
            }
        }
    }

    pub fn record_insert_outcome<BranchToken>(
        &mut self,
        outcome: &TimelineHoverPrepareInsertOutcome<BranchToken>,
    ) {
        if let TimelineHoverPrepareInsertOutcome::Inserted {
            evicted_primary_byproducts,
            ..
        } = outcome
        {
            self.record_evictions(*evicted_primary_byproducts as u64);
        }
    }

    pub fn record_pressure_release_outcome(
        &mut self,
        outcome: TimelineHoverPreparePressureReleaseOutcome,
    ) {
        match outcome {
            TimelineHoverPreparePressureReleaseOutcome::ReleasedRecentSuperseded { .. } => {
                self.record_evictions(1);
                self.released_recent_superseded = self.released_recent_superseded.saturating_add(1);
            }
            TimelineHoverPreparePressureReleaseOutcome::ReleasedPrimaryByproduct { .. } => {
                self.record_evictions(1);
                self.released_primary_byproduct = self.released_primary_byproduct.saturating_add(1);
            }
            TimelineHoverPreparePressureReleaseOutcome::NothingReleased { .. } => {
                self.pressure_release_misses = self.pressure_release_misses.saturating_add(1);
            }
        }
    }

    pub fn record_hit(&mut self) {
        self.hits = self.hits.saturating_add(1);
    }

    pub fn record_miss(&mut self) {
        self.misses = self.misses.saturating_add(1);
    }

    pub fn record_evictions(&mut self, evicted_entries: u64) {
        self.evictions = self.evictions.saturating_add(evicted_entries);
    }
}
