use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::timeline_hover_prepare::TimelineHoverPrepareTarget;
use crate::timeline_hover_source::{
    TimelineHoverOpenedSource, TimelineHoverSourceFactory, TimelineHoverSourceOpenOutcome,
};

/// App-owned guard для network hover opens: latest-only, max-one-inflight, no queue.
pub(crate) struct TimelineHoverNetworkOpenController {
    /// Generation шагает при source/config invalidation и делает старые results stale.
    source_generation: u64,

    /// Throttle между стартами network opens; `Duration::ZERO` убирает только delay.
    inter_start_throttle: Duration,

    /// Последний реальный старт background open-а.
    last_started_at: Option<Instant>,

    /// Единственный in-flight network open для текущего source generation.
    active_job: Option<TimelineHoverNetworkOpenJob>,

    /// Terminal target не auto-retry-ится, пока target/source generation не изменится.
    held_terminal_target: Option<HeldNetworkHoverTarget>,
}

impl TimelineHoverNetworkOpenController {
    /// Создаёт controller без pending jobs.
    #[must_use]
    pub(crate) fn new(inter_start_throttle: Duration) -> Self {
        Self {
            source_generation: 0,
            inter_start_throttle,
            last_started_at: None,
            active_job: None,
            held_terminal_target: None,
        }
    }

    /// Обновляет throttle из committed config; source identity при этом не меняется.
    pub(crate) fn update_inter_start_throttle(&mut self, inter_start_throttle: Duration) {
        self.inter_start_throttle = inter_start_throttle;
    }

    /// Test-only наблюдение за pending job без раскрытия storage в production API.
    #[cfg(test)]
    pub(crate) fn has_active_job(&self) -> bool {
        self.active_job.is_some()
    }

    /// Отменяет pending network work при уходе/замене hover target-а.
    pub(crate) fn cancel_pending_target(&mut self) {
        if let Some(job) = self.active_job.as_mut() {
            job.mark_stale();
        }
        self.active_job = None;
        self.held_terminal_target = None;
    }

    /// Инвалидирует pending network work при source/config boundary change.
    pub(crate) fn invalidate_source_context(&mut self) {
        self.source_generation = self.source_generation.wrapping_add(1);
        self.cancel_pending_target();
    }

    /// Пытается получить network hover source без блокировки UI/render thread-а.
    pub(crate) fn prepare_network_source(
        &mut self,
        source_factory: &TimelineHoverSourceFactory,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverNetworkOpenOutcome {
        if let Some(completed_outcome) = self.drain_completed_job() {
            return completed_outcome;
        }

        if !source_factory.active_source_is_network() {
            return TimelineHoverNetworkOpenOutcome::NonNetworkSource;
        }

        if self.held_terminal_target_matches(target) {
            return TimelineHoverNetworkOpenOutcome::FailedTargetHeld;
        }

        if let Some(job) = self.active_job.as_mut() {
            if job.target != target {
                // В текущих service APIs нет cancellation token-а, поэтому отмена выражена
                // как stale marker. Поздний result будет считан и проигнорирован.
                job.mark_stale();
            }
            return TimelineHoverNetworkOpenOutcome::Opening;
        }

        let now = Instant::now();
        if !self.throttle_allows_start(now) {
            return TimelineHoverNetworkOpenOutcome::Throttled;
        }

        let receiver = spawn_network_open(source_factory.clone());
        self.active_job = Some(TimelineHoverNetworkOpenJob {
            target,
            source_generation: self.source_generation,
            stale: false,
            receiver,
        });
        self.last_started_at = Some(now);
        TimelineHoverNetworkOpenOutcome::Opening
    }

    fn drain_completed_job(&mut self) -> Option<TimelineHoverNetworkOpenOutcome> {
        let job = self.active_job.as_mut()?;
        match job.receiver.try_recv() {
            Ok(open_outcome) => {
                let completed_job = self.active_job.take().expect("job was just observed");
                if completed_job.stale || completed_job.source_generation != self.source_generation
                {
                    return Some(TimelineHoverNetworkOpenOutcome::Opening);
                }

                Some(match open_outcome {
                    TimelineHoverSourceOpenOutcome::Opened(source) => {
                        self.held_terminal_target = None;
                        TimelineHoverNetworkOpenOutcome::Opened(source)
                    }
                    TimelineHoverSourceOpenOutcome::MissingActiveSource => {
                        TimelineHoverNetworkOpenOutcome::MissingActiveSource
                    }
                    TimelineHoverSourceOpenOutcome::Unsupported { source_kind } => {
                        self.held_terminal_target = Some(HeldNetworkHoverTarget {
                            target: completed_job.target,
                            source_generation: completed_job.source_generation,
                        });
                        TimelineHoverNetworkOpenOutcome::Unsupported { source_kind }
                    }
                    TimelineHoverSourceOpenOutcome::OpenFailed { source_kind } => {
                        self.held_terminal_target = Some(HeldNetworkHoverTarget {
                            target: completed_job.target,
                            source_generation: completed_job.source_generation,
                        });
                        TimelineHoverNetworkOpenOutcome::OpenFailed { source_kind }
                    }
                })
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let completed_job = self.active_job.take().expect("job was just observed");
                self.held_terminal_target = Some(HeldNetworkHoverTarget {
                    target: completed_job.target,
                    source_generation: completed_job.source_generation,
                });
                Some(TimelineHoverNetworkOpenOutcome::Disconnected)
            }
        }
    }

    fn held_terminal_target_matches(&self, target: TimelineHoverPrepareTarget) -> bool {
        self.held_terminal_target
            .as_ref()
            .is_some_and(|held_target| {
                held_target.target == target
                    && held_target.source_generation == self.source_generation
            })
    }

    fn throttle_allows_start(&self, now: Instant) -> bool {
        if self.inter_start_throttle.is_zero() {
            return true;
        }

        self.last_started_at
            .and_then(|last_started_at| now.checked_duration_since(last_started_at))
            .is_none_or(|elapsed| elapsed >= self.inter_start_throttle)
    }
}

/// Outcome network open controller-а; детали source kind уже сохранены в source open layer.
pub(crate) enum TimelineHoverNetworkOpenOutcome {
    NonNetworkSource,
    Opened(TimelineHoverOpenedSource),
    Opening,
    Throttled,
    MissingActiveSource,
    Unsupported {
        source_kind: crate::timeline_hover_source::TimelineHoverUnsupportedSourceKind,
    },
    OpenFailed {
        source_kind: crate::timeline_hover_source::TimelineHoverOpenFailedSourceKind,
    },
    Disconnected,
    FailedTargetHeld,
}

struct TimelineHoverNetworkOpenJob {
    target: TimelineHoverPrepareTarget,
    source_generation: u64,
    stale: bool,
    receiver: Receiver<TimelineHoverSourceOpenOutcome>,
}

impl TimelineHoverNetworkOpenJob {
    fn mark_stale(&mut self) {
        self.stale = true;
    }
}

struct HeldNetworkHoverTarget {
    target: TimelineHoverPrepareTarget,
    source_generation: u64,
}

fn spawn_network_open(
    source_factory: TimelineHoverSourceFactory,
) -> Receiver<TimelineHoverSourceOpenOutcome> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let open_outcome = source_factory.open_active_source();
        let _ = sender.send(open_outcome);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Sender};

    use frame_server_core::{
        BackendRevision, FrameExactnessPolicy, PlaybackGeneration, ScrubGeneration,
        ScrubGenerationToken, ScrubTrackSelection, SourceRevision, TimelineHoverFrameBucket,
    };
    use media_core::{TimeBase, TrackId, TrackTimestamp};
    use rustiplayer_config::PlayerDemuxConfig;

    use super::*;
    use crate::timeline_hover_prepare::{
        TimelineHoverPreparePlaybackMode, TimelineHoverPrepareTarget,
        TimelineHoverPrepareTargetContext,
    };
    use crate::timeline_hover_source::{
        TimelineHoverOpenFailedSourceKind, TimelineHoverSourceIdentity,
        TimelineHoverUnsupportedSourceKind,
    };

    fn timestamp(millis: i64) -> TrackTimestamp {
        TrackTimestamp::new(
            TrackId::new(1),
            millis,
            TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
        )
    }

    fn hover_target(target_millis: i64) -> TimelineHoverPrepareTarget {
        let context = TimelineHoverPrepareTargetContext::new(
            SourceRevision::new(1),
            BackendRevision::new(2),
            ScrubTrackSelection::video_only(TrackId::new(1)),
            ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
            FrameExactnessPolicy::TargetOrAfter,
        );

        TimelineHoverPrepareTarget::unresolved(
            context,
            timestamp(target_millis),
            TimelineHoverFrameBucket::new(target_millis),
            TimelineHoverPreparePlaybackMode::ActivePlayback,
        )
    }

    fn network_source_factory() -> TimelineHoverSourceFactory {
        let mut source_factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());
        source_factory.set_active_source(TimelineHoverSourceIdentity::DirectMediaUrl(
            "https://example.invalid/video.mp4".to_string(),
        ));
        source_factory
    }

    fn install_manual_job(
        controller: &mut TimelineHoverNetworkOpenController,
        target: TimelineHoverPrepareTarget,
    ) -> Sender<TimelineHoverSourceOpenOutcome> {
        let (sender, receiver) = mpsc::channel();
        controller.active_job = Some(TimelineHoverNetworkOpenJob {
            target,
            source_generation: controller.source_generation,
            stale: false,
            receiver,
        });
        sender
    }

    #[test]
    fn throttle_prevents_repeated_network_open_starts() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::from_secs(10));
        controller.last_started_at = Some(Instant::now());

        let outcome =
            controller.prepare_network_source(&network_source_factory(), hover_target(1000));

        assert!(matches!(
            outcome,
            TimelineHoverNetworkOpenOutcome::Throttled
        ));
        assert!(controller.active_job.is_none());
    }

    #[test]
    fn zero_throttle_still_preserves_single_inflight_without_queue() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let first_target = hover_target(1000);
        let second_target = hover_target(1200);
        let _manual_sender = install_manual_job(&mut controller, first_target);

        let outcome = controller.prepare_network_source(&network_source_factory(), second_target);

        assert!(matches!(outcome, TimelineHoverNetworkOpenOutcome::Opening));
        let active_job = controller
            .active_job
            .as_ref()
            .expect("old in-flight job stays until its late result is drained");
        assert_eq!(active_job.target, first_target);
        assert!(
            active_job.stale,
            "new target must stale-mark the old job instead of queueing another open"
        );
    }

    #[test]
    fn stale_late_result_is_ignored_without_holding_failed_target() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let original_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, original_target);
        controller
            .active_job
            .as_mut()
            .expect("manual job is installed")
            .mark_stale();
        sender
            .send(TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let outcome =
            controller.prepare_network_source(&network_source_factory(), hover_target(1200));

        assert!(matches!(outcome, TimelineHoverNetworkOpenOutcome::Opening));
        assert!(controller.active_job.is_none());
        assert!(
            controller.held_terminal_target.is_none(),
            "stale failures must not block retry of the latest target"
        );
    }

    #[test]
    fn same_failed_target_is_not_retried_automatically() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, failed_target);
        sender
            .send(TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let first_outcome =
            controller.prepare_network_source(&network_source_factory(), failed_target);
        let second_outcome =
            controller.prepare_network_source(&network_source_factory(), failed_target);

        assert!(matches!(
            first_outcome,
            TimelineHoverNetworkOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl
            }
        ));
        assert!(matches!(
            second_outcome,
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld
        ));
    }

    #[test]
    fn unsupported_target_is_not_retried_automatically() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let unsupported_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, unsupported_target);
        sender
            .send(TimelineHoverSourceOpenOutcome::Unsupported {
                source_kind: TimelineHoverUnsupportedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let first_outcome =
            controller.prepare_network_source(&network_source_factory(), unsupported_target);
        let second_outcome =
            controller.prepare_network_source(&network_source_factory(), unsupported_target);

        assert!(matches!(
            first_outcome,
            TimelineHoverNetworkOpenOutcome::Unsupported {
                source_kind: TimelineHoverUnsupportedSourceKind::DirectMediaUrl
            }
        ));
        assert!(matches!(
            second_outcome,
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld
        ));
    }

    #[test]
    fn source_invalidation_drops_pending_job_and_terminal_target() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let _manual_sender = install_manual_job(&mut controller, failed_target);
        controller.held_terminal_target = Some(HeldNetworkHoverTarget {
            target: failed_target,
            source_generation: controller.source_generation,
        });

        controller.invalidate_source_context();

        assert_eq!(controller.source_generation, 1);
        assert!(controller.active_job.is_none());
        assert!(controller.held_terminal_target.is_none());
    }

    #[test]
    fn target_cancellation_drops_pending_job_and_allows_future_retry() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let _manual_sender = install_manual_job(&mut controller, failed_target);
        controller.held_terminal_target = Some(HeldNetworkHoverTarget {
            target: failed_target,
            source_generation: controller.source_generation,
        });

        controller.cancel_pending_target();

        assert_eq!(controller.source_generation, 0);
        assert!(controller.active_job.is_none());
        assert!(controller.held_terminal_target.is_none());
    }
}
