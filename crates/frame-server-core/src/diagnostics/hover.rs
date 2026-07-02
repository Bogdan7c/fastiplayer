use std::time::Duration;

use crate::working_set::{
    TimelineHoverPrepareAdmissionOutcome, TimelineHoverPrepareNoOpReason,
    TimelineHoverPrepareProviderBudget, TimelineHoverPrepareSlotPlan,
};

use super::{CountSummary, DurationSummary};

/// Hover prepare diagnostics остаются app/player-neutral и принимают owner outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverPrepareDiagnosticsCounters {
    pub admission: ScrubHoverPrepareAdmissionCounters,
    pub dependency_span: ScrubHoverDependencySpanDiagnosticsCounters,
    pub network: ScrubHoverNetworkDiagnosticsCounters,
}

impl ScrubHoverPrepareDiagnosticsCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            admission: ScrubHoverPrepareAdmissionCounters::new(),
            dependency_span: ScrubHoverDependencySpanDiagnosticsCounters::new(),
            network: ScrubHoverNetworkDiagnosticsCounters::new(),
        }
    }
}

/// Counts hover admission/spare-capacity decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverPrepareAdmissionCounters {
    pub admitted: u64,
    pub no_op: u64,
    pub replace_existing_primary: u64,
    pub use_spare_primary_slot: u64,
    pub evict_oldest_primary_byproduct: u64,
    pub provider_spare_slot_available: u64,
    pub provider_exhausted_after_active_pins: u64,
    pub active_live_scrub_suspends_hover_prepare: u64,
    pub provider_resource_pressure: u64,
    pub no_spare_hover_slot: u64,
}

impl ScrubHoverPrepareAdmissionCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            admitted: 0,
            no_op: 0,
            replace_existing_primary: 0,
            use_spare_primary_slot: 0,
            evict_oldest_primary_byproduct: 0,
            provider_spare_slot_available: 0,
            provider_exhausted_after_active_pins: 0,
            active_live_scrub_suspends_hover_prepare: 0,
            provider_resource_pressure: 0,
            no_spare_hover_slot: 0,
        }
    }

    pub fn record_outcome(&mut self, outcome: &TimelineHoverPrepareAdmissionOutcome) {
        match outcome {
            TimelineHoverPrepareAdmissionOutcome::Admitted { slot_plan } => {
                self.admitted = self.admitted.saturating_add(1);
                self.record_slot_plan(*slot_plan);
            }
            TimelineHoverPrepareAdmissionOutcome::NoOp { reason } => {
                self.no_op = self.no_op.saturating_add(1);
                self.record_no_op_reason(*reason);
            }
        }
    }

    pub fn record_provider_budget(&mut self, provider_budget: TimelineHoverPrepareProviderBudget) {
        match provider_budget {
            TimelineHoverPrepareProviderBudget::SpareSlotAvailable => {
                self.provider_spare_slot_available =
                    self.provider_spare_slot_available.saturating_add(1);
            }
            TimelineHoverPrepareProviderBudget::ExhaustedAfterActivePins => {
                self.provider_exhausted_after_active_pins =
                    self.provider_exhausted_after_active_pins.saturating_add(1);
            }
        }
    }

    fn record_slot_plan(&mut self, slot_plan: TimelineHoverPrepareSlotPlan) {
        match slot_plan {
            TimelineHoverPrepareSlotPlan::ReplaceExistingPrimary => {
                self.replace_existing_primary = self.replace_existing_primary.saturating_add(1);
            }
            TimelineHoverPrepareSlotPlan::UseSparePrimarySlot => {
                self.use_spare_primary_slot = self.use_spare_primary_slot.saturating_add(1);
            }
            TimelineHoverPrepareSlotPlan::EvictOldestPrimaryByproduct => {
                self.evict_oldest_primary_byproduct =
                    self.evict_oldest_primary_byproduct.saturating_add(1);
            }
        }
    }

    fn record_no_op_reason(&mut self, reason: TimelineHoverPrepareNoOpReason) {
        match reason {
            TimelineHoverPrepareNoOpReason::ActiveLiveScrubSuspendsHoverPrepare => {
                self.active_live_scrub_suspends_hover_prepare = self
                    .active_live_scrub_suspends_hover_prepare
                    .saturating_add(1);
            }
            TimelineHoverPrepareNoOpReason::ProviderResourcePressure => {
                self.provider_resource_pressure = self.provider_resource_pressure.saturating_add(1);
            }
            TimelineHoverPrepareNoOpReason::NoSpareHoverSlot { .. } => {
                self.no_spare_hover_slot = self.no_spare_hover_slot.saturating_add(1);
            }
        }
    }
}

/// Typed outcome dependency span controller-а без target/source identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubHoverDependencySpanOutcome {
    Resolved,
    SameSpanRetarget,
    SpanTailExtended,
    RestartedForEarlierDecodeSafeStart,
    SupersededByIncompatibleContext,
    Incomplete(ScrubHoverDependencySpanIncompleteReason),
}

/// Почему dependency span не дал complete prepared work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubHoverDependencySpanIncompleteReason {
    DecodeExecutionNotWired,
    ResolverNotWired,
    SeekUnsupported,
    SeekUnavailable,
    ResolveFailed,
    NetworkOpening,
    NetworkThrottled,
    NetworkFailedNoRetry,
    SourceUnavailable,
    EndOfStreamBeforeTarget,
    StaleGeneration,
    ResourcePressure,
    Fatal,
}

/// Последний bounded progress одного dependency span-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverDependencySpanProgress {
    pub packets_decoded_to_target: u64,
    pub frames_decoded_to_target: u64,
    pub post_target_reorder_drain_frames: u64,
    pub prepared_targets_produced: u64,
}

/// Counts dependency span progress/outcomes без per-target истории.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverDependencySpanDiagnosticsCounters {
    pub resolved: u64,
    pub incomplete: u64,
    pub same_span_retarget: u64,
    pub span_tail_extension: u64,
    pub span_restart: u64,
    pub span_superseded: u64,
    pub packets_decoded_to_target: CountSummary,
    pub frames_decoded_to_target: CountSummary,
    pub post_target_reorder_drain_frames: CountSummary,
    pub prepared_targets_produced: CountSummary,
    pub latest_progress: Option<ScrubHoverDependencySpanProgress>,
    pub incomplete_reasons: ScrubHoverDependencySpanIncompleteReasonCounters,
}

impl ScrubHoverDependencySpanDiagnosticsCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            resolved: 0,
            incomplete: 0,
            same_span_retarget: 0,
            span_tail_extension: 0,
            span_restart: 0,
            span_superseded: 0,
            packets_decoded_to_target: CountSummary::new(),
            frames_decoded_to_target: CountSummary::new(),
            post_target_reorder_drain_frames: CountSummary::new(),
            prepared_targets_produced: CountSummary::new(),
            latest_progress: None,
            incomplete_reasons: ScrubHoverDependencySpanIncompleteReasonCounters::new(),
        }
    }

    pub fn record_outcome(&mut self, outcome: ScrubHoverDependencySpanOutcome) {
        match outcome {
            ScrubHoverDependencySpanOutcome::Resolved => {
                self.resolved = self.resolved.saturating_add(1);
            }
            ScrubHoverDependencySpanOutcome::SameSpanRetarget => {
                self.same_span_retarget = self.same_span_retarget.saturating_add(1);
            }
            ScrubHoverDependencySpanOutcome::SpanTailExtended => {
                self.span_tail_extension = self.span_tail_extension.saturating_add(1);
            }
            ScrubHoverDependencySpanOutcome::RestartedForEarlierDecodeSafeStart => {
                self.span_restart = self.span_restart.saturating_add(1);
            }
            ScrubHoverDependencySpanOutcome::SupersededByIncompatibleContext => {
                self.span_superseded = self.span_superseded.saturating_add(1);
            }
            ScrubHoverDependencySpanOutcome::Incomplete(reason) => {
                self.incomplete = self.incomplete.saturating_add(1);
                self.incomplete_reasons.increment(reason);
            }
        }
    }

    pub fn record_progress(&mut self, progress: ScrubHoverDependencySpanProgress) {
        self.packets_decoded_to_target
            .record(progress.packets_decoded_to_target);
        self.frames_decoded_to_target
            .record(progress.frames_decoded_to_target);
        self.post_target_reorder_drain_frames
            .record(progress.post_target_reorder_drain_frames);
        self.prepared_targets_produced
            .record(progress.prepared_targets_produced);
        self.latest_progress = Some(progress);
    }
}

/// Counts incomplete span reasons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverDependencySpanIncompleteReasonCounters {
    pub decode_execution_not_wired: u64,
    pub resolver_not_wired: u64,
    pub seek_unsupported: u64,
    pub seek_unavailable: u64,
    pub resolve_failed: u64,
    pub network_opening: u64,
    pub network_throttled: u64,
    pub network_failed_no_retry: u64,
    pub source_unavailable: u64,
    pub end_of_stream_before_target: u64,
    pub stale_generation: u64,
    pub resource_pressure: u64,
    pub fatal: u64,
}

impl ScrubHoverDependencySpanIncompleteReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            decode_execution_not_wired: 0,
            resolver_not_wired: 0,
            seek_unsupported: 0,
            seek_unavailable: 0,
            resolve_failed: 0,
            network_opening: 0,
            network_throttled: 0,
            network_failed_no_retry: 0,
            source_unavailable: 0,
            end_of_stream_before_target: 0,
            stale_generation: 0,
            resource_pressure: 0,
            fatal: 0,
        }
    }

    pub fn increment(&mut self, reason: ScrubHoverDependencySpanIncompleteReason) {
        match reason {
            ScrubHoverDependencySpanIncompleteReason::DecodeExecutionNotWired => {
                self.decode_execution_not_wired = self.decode_execution_not_wired.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::ResolverNotWired => {
                self.resolver_not_wired = self.resolver_not_wired.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::SeekUnsupported => {
                self.seek_unsupported = self.seek_unsupported.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::SeekUnavailable => {
                self.seek_unavailable = self.seek_unavailable.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::ResolveFailed => {
                self.resolve_failed = self.resolve_failed.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::NetworkOpening => {
                self.network_opening = self.network_opening.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::NetworkThrottled => {
                self.network_throttled = self.network_throttled.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::NetworkFailedNoRetry => {
                self.network_failed_no_retry = self.network_failed_no_retry.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::SourceUnavailable => {
                self.source_unavailable = self.source_unavailable.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::EndOfStreamBeforeTarget => {
                self.end_of_stream_before_target =
                    self.end_of_stream_before_target.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::StaleGeneration => {
                self.stale_generation = self.stale_generation.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::ResourcePressure => {
                self.resource_pressure = self.resource_pressure.saturating_add(1);
            }
            ScrubHoverDependencySpanIncompleteReason::Fatal => {
                self.fatal = self.fatal.saturating_add(1);
            }
        }
    }
}

/// App-owned network hover state, нормализованный до bounded counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubHoverNetworkState {
    NonNetworkSource,
    Opening,
    Opened,
    Throttled,
    MissingActiveSource,
    Unsupported,
    OpenFailed,
    Disconnected,
    FailedTargetHeld,
}

/// Counts latest-only network hover state без URL/source/target history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubHoverNetworkDiagnosticsCounters {
    pub non_network_source: u64,
    pub opening: u64,
    pub opened: u64,
    pub throttled: u64,
    pub missing_active_source: u64,
    pub unsupported: u64,
    pub open_failed: u64,
    pub disconnected: u64,
    pub failed_target_held: u64,
    pub zero_throttle_no_delay: u64,
    pub latest_only_replaced_in_flight: u64,
    pub stale_late_result_ignored: u64,
    pub throttle_delay: DurationSummary,
    pub latest_state: Option<ScrubHoverNetworkState>,
}

impl ScrubHoverNetworkDiagnosticsCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            non_network_source: 0,
            opening: 0,
            opened: 0,
            throttled: 0,
            missing_active_source: 0,
            unsupported: 0,
            open_failed: 0,
            disconnected: 0,
            failed_target_held: 0,
            zero_throttle_no_delay: 0,
            latest_only_replaced_in_flight: 0,
            stale_late_result_ignored: 0,
            throttle_delay: DurationSummary::new(),
            latest_state: None,
        }
    }

    pub fn record_state(&mut self, state: ScrubHoverNetworkState) {
        match state {
            ScrubHoverNetworkState::NonNetworkSource => {
                self.non_network_source = self.non_network_source.saturating_add(1);
            }
            ScrubHoverNetworkState::Opening => {
                self.opening = self.opening.saturating_add(1);
            }
            ScrubHoverNetworkState::Opened => {
                self.opened = self.opened.saturating_add(1);
            }
            ScrubHoverNetworkState::Throttled => {
                self.throttled = self.throttled.saturating_add(1);
            }
            ScrubHoverNetworkState::MissingActiveSource => {
                self.missing_active_source = self.missing_active_source.saturating_add(1);
            }
            ScrubHoverNetworkState::Unsupported => {
                self.unsupported = self.unsupported.saturating_add(1);
            }
            ScrubHoverNetworkState::OpenFailed => {
                self.open_failed = self.open_failed.saturating_add(1);
            }
            ScrubHoverNetworkState::Disconnected => {
                self.disconnected = self.disconnected.saturating_add(1);
            }
            ScrubHoverNetworkState::FailedTargetHeld => {
                self.failed_target_held = self.failed_target_held.saturating_add(1);
            }
        }
        self.latest_state = Some(state);
    }

    pub fn record_zero_throttle_no_delay(&mut self) {
        self.zero_throttle_no_delay = self.zero_throttle_no_delay.saturating_add(1);
    }

    pub fn record_latest_only_replaced_in_flight(&mut self) {
        self.latest_only_replaced_in_flight = self.latest_only_replaced_in_flight.saturating_add(1);
    }

    pub fn record_stale_late_result_ignored(&mut self) {
        self.stale_late_result_ignored = self.stale_late_result_ignored.saturating_add(1);
    }

    pub fn record_throttle_delay(&mut self, delay: Duration) {
        self.throttle_delay.record(delay);
    }
}
