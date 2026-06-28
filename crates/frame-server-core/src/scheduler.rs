use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Duration;

use crate::{
    CancelScrubIntent, CancelScrubReason, FrameServerConfig, LiveScrubDecodeMode,
    PrepareTargetIntent, ScrubIntent, ScrubPriority, ScrubRequestKind, ScrubTargetContext,
    ValidatedFrameServerConfig,
};

/// Pure scheduler state machine для future frame server-а.
///
/// Scheduler владеет только admission/order/cancellation state. Он не выполняет
/// demux seek, decode feed/drain, upload или render, а лишь выдаёт coarse scrub
/// intents внешнему owner-у.
#[derive(Debug, Clone)]
pub struct FrameScheduler {
    config: ValidatedFrameServerConfig,
    now: Duration,
    next_sequence: u64,
    active_work: Option<SchedulerWork>,
    pending_seek_landing: Option<SchedulerWork>,
    pending_live_scrub: Option<SchedulerWork>,
    pending_hover_preview: Option<SchedulerWork>,
    pending_hover_prepare_window: Vec<SchedulerWork>,
    deferred_throttled_live_scrub: Option<ScrubTargetContext>,
    latest_live_scrub_target: Option<ScrubTargetContext>,
    latest_hover_target: Option<ScrubTargetContext>,
    last_live_scrub_started_at: Option<Duration>,
}

/// Action, который внешний driver может исполнить вне этого neutral crate-а.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SchedulerAction {
    DispatchIntent(ScrubIntent),
}

/// Typed scheduler diagnostics. Это не public UI phase и не decoder lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchedulerDiagnostic {
    LiveScrubThrottled {
        latest_context: ScrubTargetContext,
        earliest_start: Duration,
    },
    ActiveWorkCancelled {
        cancelled_context: ScrubTargetContext,
        replacement_context: Option<ScrubTargetContext>,
        reason: CancelScrubReason,
    },
    HoverPrepareWindowBudgetExceeded {
        protected_context: ScrubTargetContext,
        admitted_targets: u8,
        rejected_targets: usize,
    },
    ActiveCompletionIgnored {
        completed_context: ScrubTargetContext,
        active_context: Option<ScrubTargetContext>,
    },
}

/// Результат одного scheduler boundary call-а.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerUpdate {
    pub actions: Vec<SchedulerAction>,
    pub diagnostics: Vec<SchedulerDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchedulerActiveWork {
    pub context: ScrubTargetContext,
    pub priority: ScrubPriority,
}

#[derive(Debug, Clone, Copy)]
struct SchedulerWork {
    sequence: u64,
    lane: SchedulerLane,
    priority: ScrubPriority,
    context: ScrubTargetContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SchedulerLane {
    SeekLanding,
    LiveScrub,
    HoverPreview,
    TimelineHoverPrepareWindow,
}

impl FrameScheduler {
    #[must_use]
    pub fn new(config: ValidatedFrameServerConfig) -> Self {
        Self {
            config,
            now: Duration::ZERO,
            next_sequence: 0,
            active_work: None,
            pending_seek_landing: None,
            pending_live_scrub: None,
            pending_hover_preview: None,
            pending_hover_prepare_window: Vec::new(),
            deferred_throttled_live_scrub: None,
            latest_live_scrub_target: None,
            latest_hover_target: None,
            last_live_scrub_started_at: None,
        }
    }

    pub fn submit_seek_landing_target(&mut self, context: ScrubTargetContext) -> SchedulerUpdate {
        debug_assert_eq!(context.request_kind(), ScrubRequestKind::SeekLanding);
        self.pending_seek_landing = Some(self.next_work(context));
        SchedulerUpdate::default()
    }

    pub fn submit_live_scrub_target(&mut self, context: ScrubTargetContext) -> SchedulerUpdate {
        debug_assert_eq!(context.request_kind(), ScrubRequestKind::LiveScrub);

        let mut update = SchedulerUpdate::default();
        self.latest_live_scrub_target = Some(context);
        self.suspend_hover_work_for_live_scrub(context, &mut update);
        self.cancel_active_lane_if_stale(SchedulerLane::LiveScrub, context, &mut update);

        match self.config.live_scrub_decode_mode() {
            LiveScrubDecodeMode::EveryDragEvent => {
                self.deferred_throttled_live_scrub = None;
                self.pending_live_scrub = Some(self.next_work(context));
            }
            LiveScrubDecodeMode::ThrottledLatest => {
                if self.can_start_live_scrub_at(self.now) {
                    self.deferred_throttled_live_scrub = None;
                    self.pending_live_scrub = Some(self.next_work(context));
                } else {
                    let earliest_start = self.next_live_scrub_start_at();
                    self.pending_live_scrub = None;
                    self.deferred_throttled_live_scrub = Some(context);
                    update
                        .diagnostics
                        .push(SchedulerDiagnostic::LiveScrubThrottled {
                            latest_context: context,
                            earliest_start,
                        });
                }
            }
        }

        update
    }

    pub fn submit_hover_target(&mut self, context: ScrubTargetContext) -> SchedulerUpdate {
        debug_assert_eq!(context.request_kind(), ScrubRequestKind::HoverPreview);

        let mut update = SchedulerUpdate::default();
        if self.has_live_scrub_work() {
            self.latest_hover_target = None;
            self.pending_hover_preview = None;
            return update;
        }

        self.latest_hover_target = Some(context);
        self.cancel_active_lane_if_stale(SchedulerLane::HoverPreview, context, &mut update);
        self.pending_hover_preview = Some(self.next_work(context));
        update
    }

    pub fn submit_timeline_hover_prepare_window(
        &mut self,
        protected_context: ScrubTargetContext,
        additional_hover_targets: &[ScrubTargetContext],
    ) -> SchedulerUpdate {
        debug_assert_eq!(
            protected_context.request_kind(),
            ScrubRequestKind::TimelineHoverPrepareWindow
        );

        let mut update = SchedulerUpdate::default();
        if self.has_live_scrub_work() {
            self.pending_hover_prepare_window.clear();
            return update;
        }

        self.cancel_active_lane_if_stale(
            SchedulerLane::TimelineHoverPrepareWindow,
            protected_context,
            &mut update,
        );

        self.pending_hover_prepare_window.clear();
        let protected_work = self.next_work(protected_context);
        self.pending_hover_prepare_window.push(protected_work);

        let slot_budget = usize::from(self.config.hover_prepare_window_slots());
        let optional_slot_budget = slot_budget.saturating_sub(1);
        let admitted_optional_targets = additional_hover_targets
            .iter()
            .take(optional_slot_budget)
            .copied()
            .collect::<Vec<_>>();

        for target_context in admitted_optional_targets {
            debug_assert_eq!(
                target_context.request_kind(),
                ScrubRequestKind::TimelineHoverPrepareWindow
            );
            let optional_work = self.next_work(target_context);
            self.pending_hover_prepare_window.push(optional_work);
        }

        if additional_hover_targets.len() > optional_slot_budget {
            update
                .diagnostics
                .push(SchedulerDiagnostic::HoverPrepareWindowBudgetExceeded {
                    protected_context,
                    admitted_targets: self.pending_hover_prepare_window.len() as u8,
                    rejected_targets: additional_hover_targets.len() - optional_slot_budget,
                });
        }

        update
    }

    pub fn tick(&mut self, now: Duration) -> SchedulerUpdate {
        // Scheduler time монотонный: более старый UI tick не должен открывать
        // throttle window раньше уже принятого времени.
        self.now = self.now.max(now);
        self.promote_deferred_live_scrub_if_ready();
        self.dispatch_ready_work()
    }

    pub fn complete_active_work(
        &mut self,
        completed_context: ScrubTargetContext,
    ) -> SchedulerUpdate {
        let mut update = SchedulerUpdate::default();

        match self.active_work {
            Some(active_work) if active_work.context == completed_context => {
                self.active_work = None;
            }
            Some(active_work) => {
                update
                    .diagnostics
                    .push(SchedulerDiagnostic::ActiveCompletionIgnored {
                        completed_context,
                        active_context: Some(active_work.context),
                    });
            }
            None => {
                update
                    .diagnostics
                    .push(SchedulerDiagnostic::ActiveCompletionIgnored {
                        completed_context,
                        active_context: None,
                    });
            }
        }

        update
    }

    #[must_use]
    pub fn active_work(&self) -> Option<SchedulerActiveWork> {
        self.active_work.map(|work| SchedulerActiveWork {
            context: work.context,
            priority: work.priority,
        })
    }

    #[must_use]
    pub const fn latest_live_scrub_target(&self) -> Option<ScrubTargetContext> {
        self.latest_live_scrub_target
    }

    #[must_use]
    pub const fn latest_hover_target(&self) -> Option<ScrubTargetContext> {
        self.latest_hover_target
    }

    #[cfg(test)]
    pub(crate) fn pending_work_count_for_tests(&self) -> usize {
        usize::from(self.pending_seek_landing.is_some())
            + usize::from(self.pending_live_scrub.is_some())
            + usize::from(self.pending_hover_preview.is_some())
            + self.pending_hover_prepare_window.len()
    }

    fn next_work(&mut self, context: ScrubTargetContext) -> SchedulerWork {
        let work = SchedulerWork {
            sequence: self.next_sequence,
            lane: SchedulerLane::for_request_kind(context.request_kind()),
            priority: context.priority(),
            context,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        work
    }

    fn can_start_live_scrub_at(&self, now: Duration) -> bool {
        self.last_live_scrub_started_at
            .is_none_or(|started_at| now >= started_at.saturating_add(self.live_scrub_period()))
    }

    fn next_live_scrub_start_at(&self) -> Duration {
        self.last_live_scrub_started_at
            .map_or(self.now, |started_at| {
                started_at.saturating_add(self.live_scrub_period())
            })
    }

    fn live_scrub_period(&self) -> Duration {
        let nanos_per_second = 1_000_000_000_u64;
        Duration::from_nanos(nanos_per_second / u64::from(self.config.live_scrub_max_hz()))
    }

    fn cancel_active_lane_if_stale(
        &mut self,
        lane: SchedulerLane,
        replacement_context: ScrubTargetContext,
        update: &mut SchedulerUpdate,
    ) {
        if self.active_work.is_some_and(|active_work| {
            active_work.lane == lane && active_work.context != replacement_context
        }) {
            self.cancel_active_work(Some(replacement_context), update);
        }
    }

    fn has_live_scrub_work(&self) -> bool {
        self.active_work
            .is_some_and(|active_work| active_work.lane == SchedulerLane::LiveScrub)
            || self.pending_live_scrub.is_some()
            || self.deferred_throttled_live_scrub.is_some()
    }

    fn suspend_hover_work_for_live_scrub(
        &mut self,
        live_context: ScrubTargetContext,
        update: &mut SchedulerUpdate,
    ) {
        // Во время drag-а decoder отдан live scrub, поэтому hover не должен
        // держать конкурирующую работу для того же timeline gesture.
        self.latest_hover_target = None;
        self.pending_hover_preview = None;
        self.pending_hover_prepare_window.clear();

        if self.active_work.is_some_and(|active_work| {
            matches!(
                active_work.lane,
                SchedulerLane::HoverPreview | SchedulerLane::TimelineHoverPrepareWindow
            )
        }) {
            self.cancel_active_work(Some(live_context), update);
        }
    }

    fn promote_deferred_live_scrub_if_ready(&mut self) {
        if !self.can_start_live_scrub_at(self.now) {
            return;
        }

        if let Some(context) = self.deferred_throttled_live_scrub.take() {
            self.pending_live_scrub = Some(self.next_work(context));
        }
    }

    fn dispatch_ready_work(&mut self) -> SchedulerUpdate {
        let mut update = SchedulerUpdate::default();

        if let Some(active_work) = self.active_work {
            let should_preempt = self
                .highest_pending_work()
                .is_some_and(|next_work| next_work.priority > active_work.priority);

            if should_preempt {
                self.cancel_active_work(
                    self.highest_pending_work().map(|work| work.context),
                    &mut update,
                );
            } else {
                return update;
            }
        }

        if let Some(next_work) = self.take_highest_pending_work() {
            if next_work.lane == SchedulerLane::LiveScrub {
                self.last_live_scrub_started_at = Some(self.now);
            }

            self.active_work = Some(next_work);
            update
                .actions
                .push(SchedulerAction::DispatchIntent(ScrubIntent::PrepareTarget(
                    PrepareTargetIntent {
                        context: next_work.context,
                    },
                )));
        }

        update
    }

    fn cancel_active_work(
        &mut self,
        replacement_context: Option<ScrubTargetContext>,
        update: &mut SchedulerUpdate,
    ) {
        if let Some(active_work) = self.active_work.take() {
            let reason = CancelScrubReason::SupersededByNewTarget;
            update
                .actions
                .push(SchedulerAction::DispatchIntent(ScrubIntent::Cancel(
                    CancelScrubIntent {
                        context: active_work.context,
                        reason,
                    },
                )));
            update
                .diagnostics
                .push(SchedulerDiagnostic::ActiveWorkCancelled {
                    cancelled_context: active_work.context,
                    replacement_context,
                    reason,
                });
        }
    }

    fn highest_pending_work(&self) -> Option<SchedulerWork> {
        self.pending_heap().pop()
    }

    fn take_highest_pending_work(&mut self) -> Option<SchedulerWork> {
        let next_work = self.highest_pending_work()?;
        self.remove_pending_work(next_work);
        Some(next_work)
    }

    fn pending_heap(&self) -> BinaryHeap<SchedulerWork> {
        let mut heap = BinaryHeap::new();

        if let Some(work) = self.pending_seek_landing {
            heap.push(work);
        }
        if let Some(work) = self.pending_live_scrub {
            heap.push(work);
        }
        if let Some(work) = self.pending_hover_preview {
            heap.push(work);
        }
        for work in &self.pending_hover_prepare_window {
            heap.push(*work);
        }

        heap
    }

    fn remove_pending_work(&mut self, work: SchedulerWork) {
        match work.lane {
            SchedulerLane::SeekLanding => {
                if self
                    .pending_seek_landing
                    .is_some_and(|pending| pending == work)
                {
                    self.pending_seek_landing = None;
                }
            }
            SchedulerLane::LiveScrub => {
                if self
                    .pending_live_scrub
                    .is_some_and(|pending| pending == work)
                {
                    self.pending_live_scrub = None;
                }
            }
            SchedulerLane::HoverPreview => {
                if self
                    .pending_hover_preview
                    .is_some_and(|pending| pending == work)
                {
                    self.pending_hover_preview = None;
                }
            }
            SchedulerLane::TimelineHoverPrepareWindow => {
                if let Some(index) = self
                    .pending_hover_prepare_window
                    .iter()
                    .position(|pending| *pending == work)
                {
                    self.pending_hover_prepare_window.remove(index);
                }
            }
        }
    }
}

impl SchedulerLane {
    fn for_request_kind(request_kind: ScrubRequestKind) -> Self {
        match request_kind {
            ScrubRequestKind::SeekLanding => Self::SeekLanding,
            ScrubRequestKind::LiveScrub => Self::LiveScrub,
            ScrubRequestKind::HoverPreview => Self::HoverPreview,
            ScrubRequestKind::TimelineHoverPrepareWindow => Self::TimelineHoverPrepareWindow,
        }
    }
}

impl PartialEq for SchedulerWork {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Eq for SchedulerWork {}

impl PartialOrd for SchedulerWork {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SchedulerWork {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            // BinaryHeap is a max-heap: for equal priority, older work wins.
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl Default for FrameScheduler {
    fn default() -> Self {
        let config = FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must be valid");
        Self::new(config)
    }
}
