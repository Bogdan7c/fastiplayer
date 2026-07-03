use crate::config::{FrameServerConfig, ValidatedFrameServerConfig};
use crate::request::{
    BackendRevision, CancelScrubIntent, CancelScrubReason, FeedAndDrainIntent,
    FeedAndDrainStopCondition, FinishScrubIntent, FinishScrubPolicy, PlaybackGeneration,
    PrepareTargetIntent, ScrubCurrentGuards, ScrubExactnessPolicy, ScrubGeneration,
    ScrubGenerationToken, ScrubIntent, ScrubRequestKind, ScrubTarget, ScrubTargetContext,
    ScrubTrackSelection, SeekDecodePointBeforeIntent, SourceRevision,
};
use crate::scrub::{
    CancelledOutcome, FatalOutcome, ScrubDriverOutcome, ScrubEvent, ScrubFatalReason,
};

/// Guard-часть target update без `scrub_generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubTargetUpdateGuards {
    source_revision: SourceRevision,
    backend_revision: BackendRevision,
    playback_generation: PlaybackGeneration,
}

impl ScrubTargetUpdateGuards {
    #[must_use]
    pub const fn new(
        source_revision: SourceRevision,
        backend_revision: BackendRevision,
        playback_generation: PlaybackGeneration,
    ) -> Self {
        Self {
            source_revision,
            backend_revision,
            playback_generation,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> SourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn backend_revision(self) -> BackendRevision {
        self.backend_revision
    }

    #[must_use]
    pub const fn playback_generation(self) -> PlaybackGeneration {
        self.playback_generation
    }
}

/// Driver policy, который влияет только на coarse intent payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubExecutionPolicy {
    feed_and_drain_stop_condition: FeedAndDrainStopCondition,
    finish_policy: FinishScrubPolicy,
}

impl ScrubExecutionPolicy {
    #[must_use]
    pub const fn new(
        feed_and_drain_stop_condition: FeedAndDrainStopCondition,
        finish_policy: FinishScrubPolicy,
    ) -> Self {
        Self {
            feed_and_drain_stop_condition,
            finish_policy,
        }
    }

    #[must_use]
    pub const fn driver_step_limited(
        config: ValidatedFrameServerConfig,
        finish_policy: FinishScrubPolicy,
    ) -> Self {
        Self::new(
            FeedAndDrainStopCondition::DriverStepLimit {
                max_steps: config.max_feed_and_drain_driver_steps(),
            },
            finish_policy,
        )
    }

    #[must_use]
    pub const fn feed_and_drain_stop_condition(self) -> FeedAndDrainStopCondition {
        self.feed_and_drain_stop_condition
    }

    #[must_use]
    pub const fn finish_policy(self) -> FinishScrubPolicy {
        self.finish_policy
    }
}

/// Generation-free input: machine сама поднимает `scrub_generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubTargetUpdate {
    guards: ScrubTargetUpdateGuards,
    track_selection: ScrubTrackSelection,
    target: ScrubTarget,
    exactness_policy: ScrubExactnessPolicy,
    request_kind: ScrubRequestKind,
    execution_policy: ScrubExecutionPolicy,
}

impl ScrubTargetUpdate {
    #[must_use]
    pub const fn new(
        guards: ScrubTargetUpdateGuards,
        track_selection: ScrubTrackSelection,
        target: ScrubTarget,
        exactness_policy: ScrubExactnessPolicy,
        request_kind: ScrubRequestKind,
        execution_policy: ScrubExecutionPolicy,
    ) -> Self {
        Self {
            guards,
            track_selection,
            target,
            exactness_policy,
            request_kind,
            execution_policy,
        }
    }

    #[must_use]
    pub const fn guards(self) -> ScrubTargetUpdateGuards {
        self.guards
    }

    #[must_use]
    pub const fn track_selection(self) -> ScrubTrackSelection {
        self.track_selection
    }

    #[must_use]
    pub const fn target(self) -> ScrubTarget {
        self.target
    }

    #[must_use]
    pub const fn exactness_policy(self) -> ScrubExactnessPolicy {
        self.exactness_policy
    }

    #[must_use]
    pub const fn request_kind(self) -> ScrubRequestKind {
        self.request_kind
    }

    #[must_use]
    pub const fn execution_policy(self) -> ScrubExecutionPolicy {
        self.execution_policy
    }
}

/// Текущая protocol phase: это не lifecycle decoder/demux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubProtocolPhase {
    PreparingTarget,
    SeekingDecodePoint,
    FeedingAndDraining,
    Finishing,
}

/// Один step может вернуть event и до двух coarse intents без heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubStep {
    first_intent: Option<ScrubIntent>,
    second_intent: Option<ScrubIntent>,
    event: Option<ScrubEvent>,
}

impl ScrubStep {
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            first_intent: None,
            second_intent: None,
            event: None,
        }
    }

    #[must_use]
    pub const fn from_intent(intent: ScrubIntent) -> Self {
        Self {
            first_intent: Some(intent),
            second_intent: None,
            event: None,
        }
    }

    #[must_use]
    pub const fn from_event(event: ScrubEvent) -> Self {
        Self {
            first_intent: None,
            second_intent: None,
            event: Some(event),
        }
    }

    #[must_use]
    pub const fn from_event_and_intent(event: ScrubEvent, intent: ScrubIntent) -> Self {
        Self {
            first_intent: Some(intent),
            second_intent: None,
            event: Some(event),
        }
    }

    #[must_use]
    pub const fn from_two_intents(first_intent: ScrubIntent, second_intent: ScrubIntent) -> Self {
        Self {
            first_intent: Some(first_intent),
            second_intent: Some(second_intent),
            event: None,
        }
    }

    #[must_use]
    pub const fn first_intent(self) -> Option<ScrubIntent> {
        self.first_intent
    }

    #[must_use]
    pub const fn second_intent(self) -> Option<ScrubIntent> {
        self.second_intent
    }

    #[must_use]
    pub const fn event(self) -> Option<ScrubEvent> {
        self.event
    }

    #[must_use]
    pub fn is_idle(self) -> bool {
        self.first_intent.is_none() && self.second_intent.is_none() && self.event.is_none()
    }
}

/// Чистая command/event machine: demux/decoder lifecycle исполняет внешний driver.
#[derive(Debug, Clone)]
pub struct ScrubStateMachine {
    config: ValidatedFrameServerConfig,
    current_scrub_generation: ScrubGeneration,
    active: Option<ActiveScrubState>,
}

impl ScrubStateMachine {
    #[must_use]
    pub fn new(
        config: ValidatedFrameServerConfig,
        initial_scrub_generation: ScrubGeneration,
    ) -> Self {
        Self {
            config,
            current_scrub_generation: initial_scrub_generation,
            active: None,
        }
    }

    #[must_use]
    pub const fn active_context(&self) -> Option<ScrubTargetContext> {
        match self.active {
            Some(active) => Some(active.context),
            None => None,
        }
    }

    #[must_use]
    pub const fn active_phase(&self) -> Option<ScrubProtocolPhase> {
        match self.active {
            Some(active) => Some(active.phase),
            None => None,
        }
    }

    #[must_use]
    pub const fn current_scrub_generation(&self) -> ScrubGeneration {
        self.current_scrub_generation
    }

    #[must_use]
    pub const fn config(&self) -> ValidatedFrameServerConfig {
        self.config
    }

    #[must_use]
    pub fn live_scrub_owns_target_stream(&self) -> bool {
        matches!(
            self.active,
            Some(ActiveScrubState {
                context,
                ..
            }) if context.request_kind() == ScrubRequestKind::LiveScrub
        )
    }

    pub fn submit_target_update(&mut self, update: ScrubTargetUpdate) -> ScrubStep {
        let previous_context = self.active.map(|active| active.context);
        let context = self.context_for_update(update);
        let prepare_intent = ScrubIntent::PrepareTarget(PrepareTargetIntent { context });

        self.active = Some(ActiveScrubState {
            context,
            phase: ScrubProtocolPhase::PreparingTarget,
            execution_policy: update.execution_policy(),
        });

        match previous_context {
            Some(cancelled_context) => ScrubStep::from_two_intents(
                cancel_intent(cancelled_context, CancelScrubReason::SupersededByNewTarget),
                prepare_intent,
            ),
            None => ScrubStep::from_intent(prepare_intent),
        }
    }

    pub fn handle_driver_outcome(&mut self, outcome: ScrubDriverOutcome) -> ScrubStep {
        let Some(mut active) = self.active else {
            return ScrubStep::idle();
        };

        if *outcome.context() != active.context {
            return ScrubStep::idle();
        }

        if !matches!(outcome, ScrubDriverOutcome::StaleGeneration(_))
            && outcome
                .stale_reason_against(current_guards_for_context(active.context))
                .is_some()
        {
            return ScrubStep::idle();
        }

        match outcome {
            ScrubDriverOutcome::Prepared(_) => {
                if active.phase != ScrubProtocolPhase::PreparingTarget {
                    return self.driver_invariant_failed(active.context);
                }

                active.phase = ScrubProtocolPhase::SeekingDecodePoint;
                self.active = Some(active);
                ScrubStep::from_event_and_intent(
                    ScrubEvent::from_driver_outcome(outcome),
                    ScrubIntent::SeekDecodePointBefore(SeekDecodePointBeforeIntent {
                        context: active.context,
                    }),
                )
            }
            ScrubDriverOutcome::DecodePointSeeked(_) => {
                if active.phase != ScrubProtocolPhase::SeekingDecodePoint {
                    return self.driver_invariant_failed(active.context);
                }

                active.phase = ScrubProtocolPhase::FeedingAndDraining;
                self.active = Some(active);
                ScrubStep::from_event_and_intent(
                    ScrubEvent::from_driver_outcome(outcome),
                    feed_and_drain_intent(active),
                )
            }
            ScrubDriverOutcome::Progressed(_) => {
                if active.phase != ScrubProtocolPhase::FeedingAndDraining {
                    return self.driver_invariant_failed(active.context);
                }

                self.active = Some(active);
                ScrubStep::from_event_and_intent(
                    ScrubEvent::from_driver_outcome(outcome),
                    feed_and_drain_intent(active),
                )
            }
            ScrubDriverOutcome::PreTargetReleased(_) => {
                if active.phase != ScrubProtocolPhase::FeedingAndDraining {
                    return self.driver_invariant_failed(active.context);
                }

                self.active = Some(active);
                ScrubStep::from_event_and_intent(
                    ScrubEvent::from_driver_outcome(outcome),
                    feed_and_drain_intent(active),
                )
            }
            ScrubDriverOutcome::ExactFrameReady(_) | ScrubDriverOutcome::PreviewFrameReady(_) => {
                if active.phase != ScrubProtocolPhase::FeedingAndDraining {
                    return self.driver_invariant_failed(active.context);
                }

                active.phase = ScrubProtocolPhase::Finishing;
                self.active = Some(active);
                ScrubStep::from_event_and_intent(
                    ScrubEvent::from_driver_outcome(outcome),
                    ScrubIntent::Finish(FinishScrubIntent {
                        context: active.context,
                        policy: active.execution_policy.finish_policy(),
                    }),
                )
            }
            ScrubDriverOutcome::AudioResumePending(_) => {
                if active.phase != ScrubProtocolPhase::FeedingAndDraining {
                    return self.driver_invariant_failed(active.context);
                }

                self.active = Some(active);
                ScrubStep::from_event(ScrubEvent::from_driver_outcome(outcome))
            }
            ScrubDriverOutcome::Finished(_)
            | ScrubDriverOutcome::MatchedPlayback(_)
            | ScrubDriverOutcome::Cancelled(_)
            | ScrubDriverOutcome::StaleGeneration(_)
            | ScrubDriverOutcome::ResourceBusy(_)
            | ScrubDriverOutcome::DemuxUnavailable(_)
            | ScrubDriverOutcome::DemuxUnsupported(_)
            | ScrubDriverOutcome::DecoderBackpressure(_)
            | ScrubDriverOutcome::HostUploadBackpressure(_)
            | ScrubDriverOutcome::AudioResumeTimedOut(_)
            | ScrubDriverOutcome::AudioResumeFailed(_)
            | ScrubDriverOutcome::TimedOut(_)
            | ScrubDriverOutcome::Fatal(_) => {
                self.active = None;
                ScrubStep::from_event(ScrubEvent::from_driver_outcome(outcome))
            }
        }
    }

    pub fn cancel_active(&mut self, reason: CancelScrubReason) -> ScrubStep {
        let Some(active) = self.active.take() else {
            return ScrubStep::idle();
        };

        let context = active.context;
        let event =
            ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Cancelled(CancelledOutcome {
                context,
                reason,
            }));

        ScrubStep::from_event_and_intent(event, cancel_intent(context, reason))
    }

    fn context_for_update(&mut self, update: ScrubTargetUpdate) -> ScrubTargetContext {
        let next_generation = self.next_scrub_generation();
        let guards = update.guards();
        let generation = ScrubGenerationToken::new(guards.playback_generation(), next_generation);

        ScrubTargetContext::new(
            guards.source_revision(),
            guards.backend_revision(),
            update.track_selection(),
            update.target(),
            update.exactness_policy(),
            update.request_kind(),
            generation,
        )
    }

    fn next_scrub_generation(&mut self) -> ScrubGeneration {
        let next_value = self
            .current_scrub_generation
            .get()
            .checked_add(1)
            .expect("scrub generation overflow would break latest-only guarantees");
        self.current_scrub_generation = ScrubGeneration::new(next_value);
        self.current_scrub_generation
    }

    fn driver_invariant_failed(&mut self, context: ScrubTargetContext) -> ScrubStep {
        self.active = None;
        ScrubStep::from_event(ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Fatal(
            FatalOutcome {
                context,
                reason: ScrubFatalReason::DriverInvariantViolated,
            },
        )))
    }
}

impl Default for ScrubStateMachine {
    fn default() -> Self {
        let config = FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must be valid");
        Self::new(config, ScrubGeneration::new(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ActiveScrubState {
    context: ScrubTargetContext,
    phase: ScrubProtocolPhase,
    execution_policy: ScrubExecutionPolicy,
}

fn current_guards_for_context(context: ScrubTargetContext) -> ScrubCurrentGuards {
    ScrubCurrentGuards::new(
        context.source_revision(),
        context.backend_revision(),
        context.generation(),
    )
}

fn feed_and_drain_intent(active: ActiveScrubState) -> ScrubIntent {
    ScrubIntent::FeedAndDrain(FeedAndDrainIntent {
        context: active.context,
        stop_condition: active.execution_policy.feed_and_drain_stop_condition(),
    })
}

fn cancel_intent(context: ScrubTargetContext, reason: CancelScrubReason) -> ScrubIntent {
    ScrubIntent::Cancel(CancelScrubIntent { context, reason })
}
